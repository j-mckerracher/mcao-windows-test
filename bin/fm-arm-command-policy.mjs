#!/usr/bin/env node
// Semantic policy for watcher arm and checkpoint shell commands.
//
// This parser is deliberately narrow.
// The seatbelt it serves is advisory and non-sandbox; docs/arm-pretool-check.md#advisory-status owns that status.
// It recognizes executed command positions without evaluating, expanding,
// sourcing, or running any byte of the submitted command.
//
// This file is the sole owner of firstmate's shell command classification.
// The tokenizer and command-position analysis (Lexer, splitProgram,
// commandPosition) are exported so the sibling cd-guard policy
// (bin/fm-cd-command-policy.mjs) reuses the same proven parser instead of
// duplicating shell lexing; see docs/cd-guard.md. The watcher-arm decision
// procedure below stays private to this file. The CLI entry point at the bottom
// runs only when this module is invoked directly, never on import.

import path from "node:path";
import { readFileSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

const REASONS = {
  "watcher-background": "a protected watcher command cannot run in an asynchronous shell list or through nohup/disown",
  "watcher-pipeline": "a protected watcher command must not participate in a pipeline",
  "watcher-redirection": "a protected watcher command must not use shell redirection",
  "watcher-bundled": "a protected watcher command must be the sole final command after approved setup nodes",
  "watcher-nested": "a protected watcher command must not run through a wrapper, substitution, or compound command",
  "broad-watcher-kill": "a broad process kill targeting the firstmate watcher is forbidden",
  "unclassifiable-protected-command": "unsupported or malformed shell syntax contains a protected watcher command",
  "watcher-direct": "bin/fm-watch.sh must not be run directly; arm the watcher with bin/fm-watch-arm.sh or run bin/fm-watch-checkpoint.sh instead",
};

function parseArguments(argv) {
  const result = { command: "", root: "", home: "", commandStdin: false };
  for (let i = 0; i < argv.length; i += 1) {
    const name = argv[i];
    // The command arrives on stdin so its size never meets the argument-length limit.
    if (name === "--command-stdin") {
      result.commandStdin = true;
      continue;
    }
    if (name === "--command" || name === "--root" || name === "--home") {
      if (i + 1 >= argv.length) throw new Error(`${name} requires a value`);
      result[name.slice(2)] = argv[i + 1];
      i += 1;
      continue;
    }
    throw new Error(`unknown argument: ${name}`);
  }
  return result;
}

function rawMentionsProtected(command) {
  return /(?:^|[/\s'"`(])fm-watch(?:-(?:arm|checkpoint))?\.sh\b/i.test(normalizeLineContinuations(command));
}

function rawMentionsBroadKill(command) {
  const normalized = normalizeLineContinuations(command);
  return /fm-watch/i.test(normalized) && /\b(?:pkill|kill)\b/.test(normalized);
}

function normalizeLineContinuations(source) {
  return source.replace(/\\\r?\n/g, "");
}

function basename(value) {
  return value.split("/").filter(Boolean).at(-1) || value;
}

function extractBalanced(source, start, open, close) {
  let depth = 1;
  let quote = "";
  let escaped = false;
  for (let i = start; i < source.length; i += 1) {
    const char = source[i];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (quote === "'") {
      if (char === "'") quote = "";
      continue;
    }
    if (quote === '"') {
      if (char === "\\") {
        escaped = true;
      } else if (char === '"') {
        quote = "";
      }
      continue;
    }
    if (char === "\\") {
      escaped = true;
      continue;
    }
    if (char === "'" || char === '"') {
      quote = char;
      continue;
    }
    if (char === open) depth += 1;
    if (char === close) {
      depth -= 1;
      if (depth === 0) return { content: source.slice(start, i), next: i + 1 };
    }
  }
  return null;
}

function extractBackticks(source, start) {
  let escaped = false;
  for (let i = start; i < source.length; i += 1) {
    const char = source[i];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (char === "\\") {
      escaped = true;
      continue;
    }
    if (char === "`") return { content: source.slice(start, i), next: i + 1 };
  }
  return null;
}

function decodeAnsiCQuoted(source, start) {
  let index = start + 2;
  let value = "";
  while (index < source.length) {
    const char = source[index];
    if (char === "'") return { value, next: index + 1 };
    if (char !== "\\") {
      value += char;
      index += 1;
      continue;
    }
    if (index + 1 >= source.length) return null;
    const escape = source[index + 1];
    index += 2;
    const simple = { a: "\u0007", b: "\b", e: "\u001b", E: "\u001b", f: "\f", n: "\n", r: "\r", t: "\t", v: "\v", "\\": "\\", "'": "'", '"': '"', "?": "?" };
    if (Object.hasOwn(simple, escape)) {
      value += simple[escape];
      continue;
    }
    if (/[0-7]/.test(escape)) {
      let digits = escape;
      while (digits.length < 3 && /[0-7]/.test(source[index] || "")) {
        digits += source[index];
        index += 1;
      }
      value += String.fromCodePoint(Number.parseInt(digits, 8));
      continue;
    }
    if (escape === "x") {
      let digits = "";
      while (digits.length < 2 && /[0-9A-Fa-f]/.test(source[index] || "")) {
        digits += source[index];
        index += 1;
      }
      value += digits ? String.fromCodePoint(Number.parseInt(digits, 16)) : "\\x";
      continue;
    }
    if (escape === "u" || escape === "U") {
      const length = escape === "u" ? 4 : 8;
      const digits = source.slice(index, index + length);
      if (digits.length === length && /^[0-9A-Fa-f]+$/.test(digits)) {
        const codePoint = Number.parseInt(digits, 16);
        try {
          value += String.fromCodePoint(codePoint);
          index += length;
          continue;
        } catch {}
      }
      value += `\\${escape}`;
      continue;
    }
    if (escape === "c" && index < source.length) {
      value += String.fromCodePoint(source.codePointAt(index) & 31);
      index += 1;
      continue;
    }
    value += `\\${escape}`;
  }
  return null;
}

export class Lexer {
  constructor(source) {
    this.source = source;
    this.index = 0;
    this.error = "";
    this.tokens = [];
    this.pendingHeredocs = [];
    this.expectHeredoc = null;
  }

  tokenize() {
    while (this.index < this.source.length && !this.error) {
      const char = this.source[this.index];
      if (char === " " || char === "\t" || char === "\r") {
        this.index += 1;
        continue;
      }
      if (char === "#") {
        this.skipComment();
        continue;
      }
      if (char === "\n") {
        this.tokens.push({ type: "op", value: "newline" });
        this.index += 1;
        if (this.pendingHeredocs.length > 0) this.skipHeredocBodies();
        continue;
      }
      const control = this.readControlOperator();
      if (control) {
        this.tokens.push({ type: "op", value: control });
        continue;
      }
      const redirection = this.readRedirection();
      if (redirection) {
        const token = { type: "redir", value: redirection.value, inlineTarget: redirection.inlineTarget, fd: redirection.fd };
        this.tokens.push(token);
        if (redirection.value === "<<" || redirection.value === "<<-") this.expectHeredoc = { token, stripTabs: redirection.value === "<<-" };
        continue;
      }
      // `{` is a reserved word only when whitespace follows it, as in bash; `{}` or `{a,b}` is an ordinary word
      // (for example the xargs -I replacement string), so it stays a word and is not parsed as a group.
      if (char === "(" || (char === "{" && /\s/.test(this.source[this.index + 1] || ""))) {
        const close = char === "(" ? ")" : "}";
        const balanced = extractBalanced(this.source, this.index + 1, char, close);
        if (!balanced) {
          this.error = `unclosed ${char}`;
          break;
        }
        this.tokens.push({ type: "group", kind: char === "(" ? "subshell" : "brace", content: balanced.content });
        this.index = balanced.next;
        continue;
      }
      const word = this.readWord();
      if (!word) {
        this.error = `unsupported token at byte ${this.index}`;
        break;
      }
      this.tokens.push(word);
      if (this.expectHeredoc) {
        this.pendingHeredocs.push({ delimiter: word.value, stripTabs: this.expectHeredoc.stripTabs, token: this.expectHeredoc.token });
        this.expectHeredoc = null;
      }
    }
    if (this.expectHeredoc) this.error = "missing heredoc delimiter";
    return { tokens: this.tokens, error: this.error };
  }

  skipComment() {
    while (this.index < this.source.length && this.source[this.index] !== "\n") this.index += 1;
  }

  skipHeredocBodies() {
    for (const heredoc of this.pendingHeredocs) {
      let found = false;
      let body = "";
      while (this.index < this.source.length) {
        const end = this.source.indexOf("\n", this.index);
        const lineEnd = end === -1 ? this.source.length : end;
        const line = this.source.slice(this.index, lineEnd);
        const comparable = heredoc.stripTabs ? line.replace(/^\t+/, "") : line;
        this.index = end === -1 ? this.source.length : end + 1;
        if (comparable === heredoc.delimiter) {
          found = true;
          break;
        }
        body += comparable;
        if (end !== -1) body += "\n";
      }
      if (!found) {
        this.error = "unclosed heredoc";
        break;
      }
      heredoc.token.heredoc = body;
    }
    this.pendingHeredocs = [];
  }

  readControlOperator() {
    for (const operator of ["&&", "||", "|&", ";;", ";", "&", "|"]) {
      if (this.source.startsWith(operator, this.index)) {
        this.index += operator.length;
        return operator;
      }
    }
    return "";
  }

  readRedirection() {
    const remaining = this.source.slice(this.index);
    const match = remaining.match(/^(\d+)?(<<<|<<-|<<|>>|<>|>&|<&|>|<)(?:&?[0-9-]+)?/);
    if (!match) return "";
    this.index += match[0].length;
    const inlineTarget = /(?:>&|<&)[0-9-]+$/.test(match[0]);
    let normalized = match[0].replace(/^\d+/, "");
    if (inlineTarget) normalized = normalized.replace(/[0-9-]+$/, "");
    const fd = match[1] === undefined ? (match[2].startsWith("<") ? 0 : 1) : Number(match[1]);
    return { value: normalized, inlineTarget, fd };
  }

  readWord() {
    const word = { type: "word", value: "", literal: true, subs: [], quoted: false, unquotedExpansion: false };
    let consumed = false;
    while (this.index < this.source.length) {
      const char = this.source[this.index];
      if (/\s/.test(char) || ";&|<>()".includes(char)) break;
      if (char === "#" && !consumed) break;
      consumed = true;
      if (char === "'") {
        word.quoted = true;
        const end = this.source.indexOf("'", this.index + 1);
        if (end === -1) {
          this.error = "unclosed single quote";
          return null;
        }
        word.value += this.source.slice(this.index + 1, end);
        this.index = end + 1;
        continue;
      }
      if (char === '"') {
        word.quoted = true;
        if (!this.readDoubleQuoted(word)) return null;
        continue;
      }
      if (char === "\\") {
        if (this.index + 1 >= this.source.length) {
          this.error = "trailing escape";
          return null;
        }
        if (this.source[this.index + 1] === "\n") {
          this.index += 2;
          continue;
        }
        word.value += this.source[this.index + 1];
        this.index += 2;
        continue;
      }
      if (this.source.startsWith("$'", this.index)) {
        const ansi = decodeAnsiCQuoted(this.source, this.index);
        if (!ansi) {
          this.error = "unclosed ANSI-C quote";
          return null;
        }
        word.quoted = true;
        word.value += ansi.value;
        this.index = ansi.next;
        continue;
      }
      if (this.source.startsWith('$"', this.index)) {
        word.quoted = true;
        this.index += 1;
        if (!this.readDoubleQuoted(word)) return null;
        continue;
      }
      if (this.source.startsWith("$(", this.index)) {
        const balanced = extractBalanced(this.source, this.index + 2, "(", ")");
        if (!balanced) {
          this.error = "unclosed command substitution";
          return null;
        }
        word.subs.push({ kind: "command", content: balanced.content });
        word.literal = false;
        this.index = balanced.next;
        continue;
      }
      if ((char === "<" || char === ">") && this.source[this.index + 1] === "(") {
        const balanced = extractBalanced(this.source, this.index + 2, "(", ")");
        if (!balanced) {
          this.error = "unclosed process substitution";
          return null;
        }
        word.subs.push({ kind: "process", content: balanced.content });
        word.literal = false;
        this.index = balanced.next;
        continue;
      }
      if (char === "`") {
        const backticks = extractBackticks(this.source, this.index + 1);
        if (!backticks) {
          this.error = "unclosed backtick substitution";
          return null;
        }
        word.subs.push({ kind: "command", content: backticks.content });
        word.literal = false;
        this.index = backticks.next;
        continue;
      }
      if (char === "$") word.literal = false;
      if ("*?[]{}".includes(char)) word.unquotedExpansion = true;
      word.value += char;
      this.index += 1;
    }
    return consumed ? word : null;
  }

  readDoubleQuoted(word) {
    this.index += 1;
    while (this.index < this.source.length) {
      const char = this.source[this.index];
      if (char === '"') {
        this.index += 1;
        return true;
      }
      if (char === "\\") {
        if (this.index + 1 >= this.source.length) break;
        if (this.source[this.index + 1] === "\n") {
          this.index += 2;
          continue;
        }
        // Inside double quotes a backslash escapes only $, backtick, quote, and backslash. Before any other
        // character it stays literal, so a nested $'\x62' keeps its escape for the inner shell to decode.
        if ("$`\"\\".includes(this.source[this.index + 1])) word.value += this.source[this.index + 1];
        else word.value += `\\${this.source[this.index + 1]}`;
        this.index += 2;
        continue;
      }
      if (this.source.startsWith("$(", this.index)) {
        const balanced = extractBalanced(this.source, this.index + 2, "(", ")");
        if (!balanced) break;
        word.subs.push({ kind: "command", content: balanced.content });
        word.literal = false;
        this.index = balanced.next;
        continue;
      }
      if (char === "`") {
        const backticks = extractBackticks(this.source, this.index + 1);
        if (!backticks) break;
        word.subs.push({ kind: "command", content: backticks.content });
        word.literal = false;
        this.index = backticks.next;
        continue;
      }
      if (char === "$") word.literal = false;
      word.value += char;
      this.index += 1;
    }
    this.error = "unclosed double quote";
    return false;
  }
}

export function splitProgram(tokens) {
  const nodes = [];
  const separators = [];
  let current = [];
  for (const token of tokens) {
    if (token.type === "op") {
      if (current.length > 0) {
        nodes.push(current);
        current = [];
        separators.push(token.value);
      } else if (token.value !== "newline") {
        separators.push(token.value);
      }
      continue;
    }
    current.push(token);
  }
  if (current.length > 0) nodes.push(current);
  while (separators.length >= nodes.length && separators.at(-1) === "newline") separators.pop();
  return { nodes, separators };
}

// Names whose value a non-prefix construct sets (read, printf -v, declare, for, ${X:=...}, ...).
// They are opaque everywhere in the command line, so a later literal assignment cannot make
// them look known. The key is a Symbol so it never collides with a shell variable name.
const OPAQUE_NAMES = Symbol("opaqueNames");
// Positional parameters as set by set -- and shift, or by the operands of a shell -c. An absent key means the
// line never set them. A null list means the whole list is unknown, and a null entry is a word that is not literal.
const POSITIONALS = Symbol("positionals");
const UNKNOWN_POSITIONAL_NAMES = Symbol("unknownPositionalNames");
const IDENTIFIER = /^[A-Za-z_][A-Za-z0-9_]*$/;
const NAME_SETTING_PATTERNS = [
  /\$\{([A-Za-z_][A-Za-z0-9_]*):?=/g,
  /\b(?:for|select)\s+([A-Za-z_][A-Za-z0-9_]*)/g,
  /\bprintf\s+-v\s*([A-Za-z_][A-Za-z0-9_]*)/g,
  /\bgetopts\s+\S+\s+([A-Za-z_][A-Za-z0-9_]*)/g,
];
const BUILTIN_NAME_ARGUMENTS = /\b(?:read|mapfile|readarray|declare|typeset|local|readonly|export|integer|let)\b([^;&|()\n`]*)/g;

// Scans the whole command line, including nested payloads, for names set by a non-prefix
// construct. Bare identifiers after those builtins name the variables they set; a NAME=value
// word is a plain assignment and is tracked in order instead.
function opaqueNamesOf(command) {
  const names = new Set();
  for (const pattern of NAME_SETTING_PATTERNS) {
    for (const match of command.matchAll(pattern)) names.add(match[1]);
  }
  for (const match of command.matchAll(BUILTIN_NAME_ARGUMENTS)) {
    for (const word of match[1].split(/\s+/)) {
      if (IDENTIFIER.test(word)) names.add(word);
    }
  }
  return names;
}

// Replaces each $NAME in a pattern argument with its line-local literal value. A name that is
// opaque or set from a command substitution could be anything, so it matches any text. An
// inherited name with no line-local value is the documented environment residual.
function substitutePatternVariables(value, assigned) {
  const opaque = assigned.get(OPAQUE_NAMES) || new Set();
  return value.replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$\{[^}]*\}|\$([A-Za-z_][A-Za-z0-9_]*)/g, (_, braced, bare) => {
    // Any other parameter expansion, such as ${X/a/b}, ${!X}, or ${A[0]}, has an unknowable value.
    if (braced === undefined && bare === undefined) return ".*";
    const name = braced || bare;
    const known = assigned.get(name);
    if (opaque.has(name) || known === null) return ".*";
    return typeof known === "string" ? known : "";
  });
}

// pkill, pgrep, and killall read an ERE: a backslash before X is the literal X, and POSIX
// bracket classes are valid. JavaScript RegExp reads `\c` and POSIX classes differently, so a
// pattern is checked under both readings. An unsupported construct throws and fails closed.
const POSIX_CLASSES = {
  alpha: "a-zA-Z",
  alnum: "a-zA-Z0-9",
  digit: "0-9",
  xdigit: "0-9a-fA-F",
  lower: "a-z",
  upper: "A-Z",
  word: "a-zA-Z0-9_",
  space: " \\t\\n\\r\\f\\v",
  punct: "!-\\/:-@\\[-`{-~",
};

function escapeForJs(char) {
  return /[.*+?^${}()|[\]\\/-]/.test(char) ? `\\${char}` : char;
}

function eresToJs(text) {
  let out = "";
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if (char === "\\" && index + 1 < text.length) {
      index += 1;
      out += escapeForJs(text[index]);
      continue;
    }
    if (char !== "[") {
      out += char;
      continue;
    }
    let body = "";
    let cursor = index + 1;
    if (text[cursor] === "^") {
      body += "^";
      cursor += 1;
    }
    if (text[cursor] === "]") {
      body += "\\]";
      cursor += 1;
    }
    let closed = false;
    while (cursor < text.length) {
      if (text[cursor] === "]") {
        closed = true;
        break;
      }
      if (text[cursor] === "[" && text[cursor + 1] === ":") {
        const end = text.indexOf(":]", cursor + 2);
        if (end === -1) throw new Error("unterminated POSIX class");
        const cls = POSIX_CLASSES[text.slice(cursor + 2, end)];
        if (cls === undefined) throw new Error("unsupported POSIX class");
        body += cls;
        cursor = end + 2;
        continue;
      }
      if (text[cursor] === "[" && (text[cursor + 1] === "." || text[cursor + 1] === "=")) {
        throw new Error("unsupported collating element");
      }
      if (text[cursor] === "\\") body += "\\\\";
      else if (text[cursor] === "[") body += "\\[";
      else body += text[cursor];
      cursor += 1;
    }
    if (!closed) throw new Error("unterminated bracket expression");
    out += `[${body}]`;
    index = cursor;
  }
  return out;
}

// A pattern argument names the watcher when either reading matches a protected basename. A
// pattern that cannot be read under either reading fails closed.
function namesWatcherPattern(value, assigned = new Map()) {
  const text = substitutePatternVariables(value, assigned);
  let sources;
  try {
    sources = [text, eresToJs(text)];
  } catch {
    return true;
  }
  for (const source of sources) {
    let pattern;
    try {
      pattern = new RegExp(source, "i");
    } catch {
      return true;
    }
    if (PROTECTED_BASENAMES.some((name) => pattern.test(name))) return true;
  }
  return false;
}

// A kill-family or selector argument names the watcher when it contains fm-watch literally, when
// a command substitution could produce any value, or when a non-option word matches a protected
// basename or a line-local name resolves to watcher text. Options are never patterns.
function wordNamesWatcher(word, context) {
  if (word.subs.length > 0) return true;
  if (/fm-watch/i.test(word.value)) return true;
  if (word.value.startsWith("-")) return false;
  return namesWatcherPattern(word.value, context.assigned) || wordReferencesAny(word, context.watcherPatterns);
}

const KILL_COMMANDS = new Set(["kill", "killall", "pkill", "fuser"]);
const SELECTOR_COMMANDS = new Set(["grep", "egrep", "fgrep", "rg", "ag", "ack", "pgrep", "awk", "lsof", "fuser", "pidof"]);

// Splits nodes into pipelines. When separators do not align one-to-one with nodes, the whole
// line is one group, which only widens what is checked.
function pipelineGroups(nodeInfos, separators) {
  if (nodeInfos.length === 0) return [];
  if (separators.length !== nodeInfos.length - 1) return [nodeInfos];
  const groups = [];
  let current = [nodeInfos[0]];
  for (let index = 1; index < nodeInfos.length; index += 1) {
    const separator = separators[index - 1];
    if (separator === "|" || separator === "|&") {
      current.push(nodeInfos[index]);
    } else {
      groups.push(current);
      current = [nodeInfos[index]];
    }
  }
  groups.push(current);
  return groups;
}

// A relative path resolves against the current directory, which a setup `cd` changes. A `~`,
// `$`-led, or absolute path does not depend on the current directory.
function isRelativePath(value) {
  return value.includes("/") && !value.startsWith("/") && !value.startsWith("~") && !value.startsWith("$");
}

function trimTrailingSlashes(value) {
  return value.length > 1 ? value.replace(/\/+$/, "") : value;
}

function isAssignment(value) {
  return /^[A-Za-z_][A-Za-z0-9_]*=/.test(value);
}

function wordsInNode(tokens) {
  const words = [];
  let skipRedirectionTarget = false;
  for (const token of tokens) {
    if (token.type === "redir") {
      skipRedirectionTarget = !token.inlineTarget;
      continue;
    }
    if (skipRedirectionTarget && token.type === "word") {
      skipRedirectionTarget = false;
      continue;
    }
    if (token.type === "word") words.push(token);
  }
  return words;
}

const WRAPPER_OPTIONS = {
  command: { noArgument: new Set(["p", "v", "V"]), takesArgument: new Set() },
  // macOS env takes `-P utilpath`; GNU env rejects -P outright, so taking an argument is safe on both.
  env: { noArgument: new Set(["0", "i", "v"]), takesArgument: new Set(["a", "C", "P", "S", "u"]) },
  exec: { noArgument: new Set(["c", "l"]), takesArgument: new Set(["a"]) },
  nohup: { noArgument: new Set(), takesArgument: new Set() },
  sudo: { noArgument: new Set(["A", "B", "b", "E", "e", "H", "i", "K", "k", "l", "N", "n", "P", "S", "s", "v", "V"]), takesArgument: new Set(["C", "D", "g", "h", "p", "r", "R", "t", "T", "u", "U"]) },
  timeout: { noArgument: new Set(["f", "p", "v"]), takesArgument: new Set(["k", "s"]) },
};

const WRAPPER_LONG_OPTIONS = {
  command: { noArgument: new Set(["help", "version"]), takesArgument: new Set() },
  env: { noArgument: new Set(["ignore-environment", "null", "help", "version"]), takesArgument: new Set(["argv0", "block-signal", "chdir", "default-signal", "ignore-signal", "split-string", "unset"]) },
  exec: { noArgument: new Set(), takesArgument: new Set() },
  nohup: { noArgument: new Set(["help", "version"]), takesArgument: new Set() },
  sudo: { noArgument: new Set(["askpass", "background", "bell", "edit", "help", "login", "non-interactive", "preserve-env", "preserve-groups", "remove-timestamp", "reset-timestamp", "set-home", "shell", "stdin", "validate", "version"]), takesArgument: new Set(["chdir", "chroot", "close-from", "command-timeout", "group", "host", "other-user", "prompt", "role", "type", "user"]) },
  timeout: { noArgument: new Set(["foreground", "preserve-status", "verbose", "help", "version"]), takesArgument: new Set(["kill-after", "signal"]) },
};

function consumeWrapperOptions(name, words, index) {
  const optionOwner = name === "gtimeout" ? "timeout" : name;
  const short = WRAPPER_OPTIONS[optionOwner];
  const long = WRAPPER_LONG_OPTIONS[optionOwner];
  const embeddedPayloads = [];
  let next = index;
  while (words[next]) {
    const value = words[next].value;
    if (value === "--") return { index: next + 1, unresolved: false, embeddedPayloads };
    // GNU env accepts a bare `-` before any assignment as its -i synonym.
    if (name === "env" && value === "-") {
      next += 1;
      continue;
    }
    if (!value.startsWith("-") || value === "-") return { index: next, unresolved: false, embeddedPayloads };
    if (value.startsWith("--")) {
      const equals = value.indexOf("=");
      const option = value.slice(2, equals === -1 ? undefined : equals);
      if (long.noArgument.has(option)) {
        next += 1;
        continue;
      }
      if (!long.takesArgument.has(option)) return { index: next, unresolved: true, embeddedPayloads };
      if (equals !== -1) {
        if (name === "env" && option === "split-string") embeddedPayloads.push(value.slice(equals + 1));
        next += 1;
        continue;
      }
      if (!words[next + 1]) return { index: next, unresolved: true, embeddedPayloads };
      if (name === "env" && option === "split-string") embeddedPayloads.push(words[next + 1].value);
      next += 2;
      continue;
    }
    let consumedArgument = false;
    for (let offset = 1; offset < value.length; offset += 1) {
      const option = value[offset];
      if (short.noArgument.has(option)) continue;
      if (!short.takesArgument.has(option)) return { index: next, unresolved: true, embeddedPayloads };
      if (offset + 1 === value.length) {
        if (!words[next + 1]) return { index: next, unresolved: true, embeddedPayloads };
        if (name === "env" && option === "S") embeddedPayloads.push(words[next + 1].value);
        next += 2;
      } else {
        if (name === "env" && option === "S") embeddedPayloads.push(value.slice(offset + 1));
        next += 1;
      }
      consumedArgument = true;
      break;
    }
    if (!consumedArgument) next += 1;
  }
  return { index: next, unresolved: false, embeddedPayloads };
}

const XARGS_NO_ARGUMENT = new Set(["0", "e", "i", "l", "o", "p", "r", "t", "x"]);
const XARGS_TAKES_ARGUMENT = new Set(["a", "d", "E", "I", "L", "n", "P", "s"]);
const XARGS_LONG_ARGUMENT = new Set(["arg-file", "delimiter", "max-args", "max-chars", "max-lines", "max-procs", "process-slot-var"]);
const XARGS_LONG_NO_ARGUMENT = new Set(["eof", "exit", "help", "interactive", "no-run-if-empty", "null", "open-tty", "replace", "show-limits", "verbose", "version"]);

const XARGS_LONG_OPTIONS = [...XARGS_LONG_ARGUMENT, ...XARGS_LONG_NO_ARGUMENT];

// GNU getopt_long accepts any unambiguous prefix of a long option name. Returns the full name, or null when the
// prefix matches no option or more than one, which xargs refuses.
function resolveXargsLongOption(name) {
  if (XARGS_LONG_OPTIONS.includes(name)) return name;
  const matches = XARGS_LONG_OPTIONS.filter((option) => option.startsWith(name));
  return matches.length === 1 ? matches[0] : null;
}

// xargs runs its first non-option word as the launched command, so it is a wrapper. Unknown
// options fail closed as unresolved, since they could hide which word is the command.
function consumeXargsOptions(words, index) {
  let next = index;
  while (words[next]) {
    const value = words[next].value;
    if (value === "--") return { index: next + 1, unresolved: false };
    if (!value.startsWith("-") || value === "-") return { index: next, unresolved: false };
    if (value.startsWith("--")) {
      const equals = value.indexOf("=");
      const option = resolveXargsLongOption(value.slice(2, equals === -1 ? undefined : equals));
      if (option === null) return { index: next, unresolved: true };
      if (XARGS_LONG_ARGUMENT.has(option) && equals === -1) {
        if (!words[next + 1]) return { index: next, unresolved: true };
        next += 2;
        continue;
      }
      next += 1;
      continue;
    }
    let consumedArgument = false;
    for (let offset = 1; offset < value.length; offset += 1) {
      const option = value[offset];
      if (XARGS_NO_ARGUMENT.has(option)) continue;
      if (!XARGS_TAKES_ARGUMENT.has(option)) return { index: next, unresolved: true };
      if (offset + 1 === value.length) {
        if (!words[next + 1]) return { index: next, unresolved: true };
        next += 2;
      } else {
        next += 1;
      }
      consumedArgument = true;
      break;
    }
    if (!consumedArgument) next += 1;
  }
  return { index: next, unresolved: false };
}

export function commandPosition(tokens) {
  const words = wordsInNode(tokens);
  let index = 0;
  while (index < words.length && isAssignment(words[index].value)) index += 1;
  const prefixAssignments = index;
  const wrappers = [];
  let unresolvedWrapperOption = false;
  const wrapperPayloads = [];
  let command = words[index];
  while (command) {
    const name = basename(command.value);
    if (name === "exec" || name === "command" || name === "sudo" || name === "nohup") {
      wrappers.push(name);
      const options = consumeWrapperOptions(name, words, index + 1);
      unresolvedWrapperOption ||= options.unresolved;
      wrapperPayloads.push(...options.embeddedPayloads);
      index = options.index;
      command = words[index];
      continue;
    }
    if (name === "env") {
      wrappers.push(name);
      const options = consumeWrapperOptions(name, words, index + 1);
      unresolvedWrapperOption ||= options.unresolved;
      wrapperPayloads.push(...options.embeddedPayloads);
      index = options.index;
      while (words[index] && isAssignment(words[index].value)) index += 1;
      // GNU and BSD env both stop option parsing at the first assignment; a dash word after it is
      // the utility on GNU but may be read as an option on BSD, so it is unresolved and fails closed
      // when a protected name is present.
      if (words[index] && words[index].value.startsWith("-")) unresolvedWrapperOption = true;
      command = words[index];
      continue;
    }
    if (name === "xargs") {
      wrappers.push(name);
      const options = consumeXargsOptions(words, index + 1);
      unresolvedWrapperOption ||= options.unresolved;
      index = options.index;
      command = words[index];
      continue;
    }
    if (name === "timeout" || name === "gtimeout") {
      wrappers.push(name);
      const options = consumeWrapperOptions(name, words, index + 1);
      unresolvedWrapperOption ||= options.unresolved;
      index = options.index;
      if (!words[index]) {
        unresolvedWrapperOption = true;
        command = undefined;
        break;
      }
      index += 1;
      command = words[index];
      if (!command) {
        unresolvedWrapperOption = true;
        break;
      }
      continue;
    }
    break;
  }
  return { words, index, command, wrappers, prefixAssignments, unresolvedWrapperOption, wrapperPayloads };
}

const PROTECTED_SCRIPTS = [
  { relative: "bin/fm-watch-arm.sh", kind: "arm" },
  { relative: "bin/fm-watch-checkpoint.sh", kind: "checkpoint" },
  { relative: "bin/fm-watch.sh", kind: "watch" },
  // The bare name `fm-watch` is what PATH resolves for an installed-looking command; it has no
  // installed file, so it is protected by identity alone (DPES-893 AC8).
  { relative: "bin/fm-watch", kind: "bare" },
];

// Identity is case-folded because the default macOS volume is case-insensitive, so
// FM-WATCH-ARM.SH resolves to the same watcher file as fm-watch-arm.sh.
function protectedIdentity(value, root) {
  const normalized = path.normalize(value).toLowerCase();
  for (const { relative, kind } of PROTECTED_SCRIPTS) {
    const script = relative.toLowerCase();
    // A bare script name reaches the watcher through PATH, so it is the same identity.
    if (normalized === script || normalized === path.join(root, script).toLowerCase() || normalized.endsWith(`/${script}`) || normalized === path.basename(script)) return kind;
  }
  return "";
}

// Parameter expansions can supply a watcher name, so the word is also read with each
// ${NAME:-WORD} or ${NAME+WORD} form replaced by WORD and each $NAME removed.
function expansionSkeleton(value) {
  return value
    .replace(/\$\{[A-Za-z_][A-Za-z0-9_]*(?::?[-+=?]([^{}]*))?\}/g, (_, word = "") => word)
    .replace(/\$[A-Za-z_][A-Za-z0-9_]*/g, "");
}

// A variable can supply the leading part of a watcher name, as in $X-watch-arm.sh.
const DYNAMIC_WATCHER_NAME = /fm-watch|(?:^|[-_/])watch(?:-(?:arm|checkpoint))?\.sh/i;

function hasUnclassifiableProtectedExpansion(word, root) {
  if (!word || protectedIdentity(word.value, root)) return false;
  if (!word.unquotedExpansion && !word.value.includes("$")) return false;
  return DYNAMIC_WATCHER_NAME.test(word.value) || DYNAMIC_WATCHER_NAME.test(expansionSkeleton(word.value));
}

// Watcher identity names a file this policy protects. A glob, brace, command substitution,
// or variable in command position can produce one of these basenames, so the policy
// tests the expansion against each basename instead of only the literal fragment.
const PROTECTED_BASENAMES = ["fm-watch-arm.sh", "fm-watch-checkpoint.sh", "fm-watch.sh", "fm-watch"];

// Only the approved arm and checkpoint identities may stand as the sole final command.
const FINAL_ALLOWED_KINDS = new Set(["arm", "checkpoint"]);

// Setup `export` of a variable that changes which executable a bare name resolves to, or
// which startup code a child shell loads, is not approved setup.
// The loader, module, and exec families (PERL*, RUBY*, NODE_*, PYTHON*, GIT_*) and HOME change
// which code a later protected call loads, or which file a `~` path names, so they are refused too.
const UNSAFE_SETUP_VARIABLE = /^(?:PATH|CDPATH|BASH_ENV|ENV|IFS|SHELLOPTS|BASHOPTS|PS4|PROMPT_COMMAND|HOME|ZDOTDIR|LD_\w*|DYLD_\w*|PERL\w*|RUBY\w*|NODE_\w*|PYTHON\w*|GIT_\w*)$/;

// Glob and brace metacharacters in a positional or assigned value. Unquoted, the shell expands them to paths
// that can name a protected script the literal text does not, so such a value is unknown where it is run.
const GLOB_OR_BRACE = /[*?[{]/;

function newExpansion() {
  return { source: "", text: "", dynamic: false, glob: false, literalChars: 0, failClosed: false, opaque: false, unknownPositional: false };
}

// Expands $N, ${N}, $@, and $* from the positional list the line tracks. A value that is unknown, or that
// may name a watcher, is flagged so command-position, eval, and source use of it fails closed.
function expandPositional(ref, assigned, depth, state) {
  const list = assigned.get(POSITIONALS);
  if (list === undefined || list === null) {
    state.unknownPositional = true;
    return appendWildcard(state, ".*");
  }
  if (ref === "@" || ref === "*") {
    if (list.some((value) => value === null || mentionsWatcherText(value) || GLOB_OR_BRACE.test(value))) state.unknownPositional = true;
    return appendWildcard(state, ".*");
  }
  const value = list[Number(ref) - 1];
  if (value === undefined) return appendWildcard(state, ".*");
  if (value === null) {
    state.unknownPositional = true;
    return appendWildcard(state, ".*");
  }
  if (GLOB_OR_BRACE.test(value)) state.unknownPositional = true;
  if (depth < 8) expandWord(value, assigned, depth + 1, state);
  else appendWildcard(state, ".*");
}

function appendWildcard(state, source) {
  state.dynamic = true;
  state.source += source;
}

function matchingBraceIndex(text, open) {
  let depth = 0;
  for (let index = open; index < text.length; index += 1) {
    if (text[index] === "{") depth += 1;
    else if (text[index] === "}") {
      depth -= 1;
      if (depth === 0) return index;
    }
  }
  return -1;
}

function topLevelCommas(body) {
  const parts = [];
  let depth = 0;
  let start = 0;
  for (let index = 0; index < body.length; index += 1) {
    if (body[index] === "{") depth += 1;
    else if (body[index] === "}") depth -= 1;
    else if (body[index] === "," && depth === 0) {
      parts.push(body.slice(start, index));
      start = index + 1;
    }
  }
  parts.push(body.slice(start));
  return parts;
}

// Expands the shell text of one word into the regular-expression source of every string it
// could produce. Variables assigned earlier in the same command line resolve to their literal
// values; any other expansion is unknown and matches any text.
function expandWord(text, assigned, depth = 0, state = newExpansion()) {
  let index = 0;
  while (index < text.length) {
    const char = text[index];
    if (char === "$") {
      const bracedPositional = /^\$\{(\d+|[@*])([^}]*)\}/.exec(text.slice(index));
      if (bracedPositional) {
        index += bracedPositional[0].length;
        const ref = bracedPositional[1];
        if (bracedPositional[2]) {
          state.unknownPositional = true;
          state.opaque = true;
          appendWildcard(state, ".*");
        } else {
          expandPositional(ref, assigned, depth, state);
        }
        continue;
      }
      const positional = /^\$(\d+|[@*])/.exec(text.slice(index));
      if (positional) {
        index += positional[0].length;
        expandPositional(positional[1], assigned, depth, state);
        continue;
      }
      const named = /^\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))/.exec(text.slice(index));
      if (named) {
        index += named[0].length;
        const name = named[1] || named[2];
        const known = assigned.get(name);
        if (typeof known === "string" && GLOB_OR_BRACE.test(known)) state.unknownPositional = true;
        // A name set by a non-prefix construct, or assigned from a command substitution (null), has
        // an unknowable value here, so a command-position use may name a protected script and fails closed.
        if (assigned.get(UNKNOWN_POSITIONAL_NAMES)?.has(name)) {
          state.unknownPositional = true;
          appendWildcard(state, ".*");
        } else if (assigned.get(OPAQUE_NAMES)?.has(name) || known === null) {
          state.opaque = true;
          appendWildcard(state, ".*");
        } else if (known !== undefined && depth < 8) expandWord(known, assigned, depth + 1, state);
        else appendWildcard(state, ".*");
        continue;
      }
      // Indirection, ${!1} or ${!NAME}, expands to the variable a value names. The classifier cannot see that
      // variable, so the expansion is unknown and fails closed at an execution sink.
      if (text.startsWith("${!", index)) {
        const close = text.indexOf("}", index);
        index = close === -1 ? text.length : close + 1;
        state.unknownPositional = true;
        state.opaque = true;
        appendWildcard(state, ".*");
        continue;
      }
      // A parameter expansion other than a plain ${NAME}, such as ${NAME:-word}, ${NAME/pat/rep}, or ${NAME[0]},
      // yields a value the classifier does not resolve, so it fails closed in command position. Special parameters
      // such as $1 or $? are unknown and match any text.
      const close = text.startsWith("${", index) ? text.indexOf("}", index) : -1;
      if (close !== -1) state.opaque = true;
      index = close === -1 ? index + 1 : close + 1;
      appendWildcard(state, ".*");
      continue;
    }
    if (char === "*" || char === "?" || (char === "[" && text.indexOf("]", index + 1) !== -1)) {
      state.glob = true;
      const close = char === "[" ? text.indexOf("]", index + 1) : index;
      appendWildcard(state, char === "*" ? ".*" : ".");
      index = close + 1;
      continue;
    }
    if (char === "{") {
      const close = matchingBraceIndex(text, index);
      if (close !== -1) {
        const body = text.slice(index + 1, close);
        // A brace group containing a slash could move the basename split; refuse it.
        if (body.includes("/")) state.failClosed = true;
        if (body.includes("..")) {
          state.glob = true;
          appendWildcard(state, ".*");
          index = close + 1;
          continue;
        }
        const alternatives = topLevelCommas(body);
        if (alternatives.length > 1) {
          state.glob = true;
          state.dynamic = true;
          const parts = alternatives.map((alt) => expandWord(alt, assigned, depth + 1));
          if (parts.some((part) => part.opaque)) state.opaque = true;
          if (parts.some((part) => part.unknownPositional)) state.unknownPositional = true;
          state.source += `(?:${parts.map((part) => part.source).join("|")})`;
          index = close + 1;
          continue;
        }
      }
    }
    state.text += char;
    state.source += char.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
    if (/[A-Za-z0-9]/.test(char)) state.literalChars += 1;
    index += 1;
  }
  return state;
}

// True when a command-position word could expand to a protected script basename. A command
// substitution's output is unknowable, so it always counts. An unknown expansion with no
// literal text (for example a bare inherited $VAR) is the documented inherited-variable residual.
function wordCouldNameProtectedScript(word, assigned, root) {
  if (!word) return false;
  if (word.subs.length > 0) return true;
  // A word that already names a protected path, such as $FM_HOME/bin/fm-watch-arm.sh, is
  // classified by that identity; only words the identity cannot see are tested here.
  if (protectedIdentity(word.value, root)) return false;
  const full = expandWord(word.value, assigned);
  if (full.failClosed || full.opaque || full.unknownPositional) return true;
  if (!full.dynamic) return false;
  const last = expandWord(word.value.slice(word.value.lastIndexOf("/") + 1), assigned);
  if (last.failClosed) return true;
  if (!last.glob && last.literalChars === 0) return false;
  const pattern = new RegExp(`^${last.source}$`, "i");
  return PROTECTED_BASENAMES.some((name) => pattern.test(name));
}

// Shells whose `-c` operand is a program text that is classified like sh's. Any other shell is an unmodeled executor.
const SHELL_NAMES = new Set(["sh", "bash", "zsh", "dash", "ksh", "ksh93", "mksh", "oksh", "pdksh", "ash", "yash", "posh", "fish", "csh", "tcsh"]);

// Options that take an argument in bash and POSIX shells, in their long form.
const SHELL_LONG_OPTIONS_WITH_ARGUMENT = new Set(["--init-file", "--rcfile"]);

// A shell option word. `-o`, `+o`, `-O`, and `+O` take the next word as their argument unless the rest of
// the cluster is attached, and `-c` before that point names the command string. Returns null for a word
// that is not an option.
function shellOptionWord(value) {
  if (SHELL_LONG_OPTIONS_WITH_ARGUMENT.has(value)) return { command: false, takesNext: true };
  if (!/^[-+][A-Za-z]+$/.test(value)) return null;
  let command = false;
  for (let k = 1; k < value.length; k += 1) {
    const letter = value[k];
    if (letter === "o" || letter === "O") return { command, takesNext: k + 1 === value.length };
    if (letter === "c" && value[0] === "-") command = true;
  }
  return { command, takesNext: false };
}

// Commands that run an operand as a command line and whose option grammar this parser does not model.
// A protected watcher name anywhere in such a node fails closed as nested, because the name may be the
// executed operand behind an option this parser does not read. Read-only data arguments are not on this
// list, so a benign mention under them stays allowed.
const UNMODELED_EXECUTORS = new Set(["alacritty", "arch", "asdf", "at", "bundle", "bunx", "busybox", "cgexec", "chronic", "chrt", "chroot", "conda", "corepack", "cross-env", "dbus-run-session", "devbox", "direnv", "doas", "docker", "dotenvx", "doppler", "elvish", "entr", "env-cmd", "expect", "fd", "fdfind", "firejail", "flock", "gdb", "gnome-terminal", "hatch", "ion", "ionice", "konsole", "kitty", "kubectl", "launchctl", "ltrace", "make", "mise", "newgrp", "nix", "nix-shell", "npx", "nsenter", "nu", "numactl", "open", "op", "osascript", "osh", "parallel", "pdm", "perf", "pipenv", "pkexec", "pnpx", "podman", "poetry", "powershell", "prlimit", "pwsh", "rye", "runuser", "screen", "script", "setarch", "setpriv", "setsid", "sg", "sshpass", "ssh", "strace", "su", "systemd-run", "taskset", "unbuffer", "unshare", "uv", "valgrind", "watchexec", "wezterm", "xonsh", "xterm"]);
// Interpreters whose inline program (-c, -e, a script on stdin, or an awk program) can run a shell command.
// A protected name in their node fails closed as nested, the same as an unmodeled executor.
const INLINE_CODE_INTERPRETERS = /^(?:awk|bun|coffee|deno|elixir|erl|gawk|groovy|guile|ipython\d*|julia|lua\d*(?:\.\d+)*|luajit[\d.]*|mawk|nawk|node(?:js)?\d*(?:\.\d+)*|perl\d*(?:\.\d+)*|php\d*(?:\.\d+)*|pypy\d*|python\d*(?:\.\d+)?|R|Rscript|ruby\d*(?:\.\d+)*|scala|tclsh(?:\d+(?:\.\d+)?)?|ts-node|tsx|wish)$/;
// Editors that run an ex, ed, or shell command from their arguments (`-c '!cmd'`, `--eval`) or from stdin (`ex -s`,
// `ed -s`) are unmodeled executors. Matched by name family so version, variant, and alias names (vim.basic, nvim-qt,
// gview) are covered.
const EDITOR_EXECUTORS = /^(?:[gnr]?vim(?:x|diff)?|[gnr]?view|[gnr]?vi|[gnr]?ex|[gr]?ed|emacs(?:client)?|xemacs)(?:[.-][\w.+-]*|\d[\w.+-]*)?$/;
const FIND_EXECUTING_FLAGS = new Set(["-exec", "-execdir", "-ok", "-okdir"]);
const TMUX_EXECUTING_SUBCOMMANDS = new Set(["if", "if-shell", "neww", "new", "new-session", "new-window", "pipe-pane", "pipep", "respawn-pane", "respawnp", "run", "run-shell", "send", "send-keys", "split-window", "splitw"]);
const PACKAGE_EXEC_SUBCOMMANDS = new Set(["exec", "dlx"]);

// Whether a node's command word runs one of its operands as a command line.
function runsOperandCommand(position) {
  if (!position.command) return false;
  const name = basename(position.command.value);
  const args = position.words.slice(position.index + 1).map((word) => word.value);
  // BSD env reads `--name` as a short-option cluster (`--unset X` is `-u nset` with X as the utility),
  // so the operand after a long env option has a platform-dependent position and fails closed.
  if (position.wrappers.includes("env") && position.words.slice(0, position.index).some((word) => word.value.startsWith("--") && word.value !== "--")) return true;
  if (UNMODELED_EXECUTORS.has(name)) return true;
  if (EDITOR_EXECUTORS.test(name)) return true;
  if (INLINE_CODE_INTERPRETERS.test(name)) return true;
  if (name === "find") return args.some((value) => FIND_EXECUTING_FLAGS.has(value));
  if (name === "git") return args.some((value) => value.startsWith("-c")) || gitRunsOperand(args);
  if (name === "npm") return args.some((value) => value === "exec" || value === "x");
  if (name === "pnpm" || name === "yarn") return args.some((value) => PACKAGE_EXEC_SUBCOMMANDS.has(value));
  if (name === "sed") return sedRunsScript(args);
  if (name === "tmux") return args.some((value) => TMUX_EXECUTING_SUBCOMMANDS.has(value));
  // trap runs its action operand as a shell command line when the signal fires.
  if (name === "trap") return true;
  return false;
}

// git accepts any unambiguous prefix of a long option (--exe is --exec), so a long option word that is a
// prefix of --exec, with at least `--e`, counts as --exec.
function isGitExecAbbreviation(value) {
  const name = value.split("=")[0];
  return name.length >= 3 && name.startsWith("--") && "--exec".startsWith(name);
}

// git rebase --exec, git submodule foreach, and git bisect run each run an operand as a shell command line.
// git filter-branch and filter-repo run their filter operands through the shell, and git difftool -x runs its command.
function gitRunsOperand(args) {
  if (args.includes("rebase") && args.some((value) => isGitExecAbbreviation(value) || value.startsWith("-x"))) return true;
  if (args.includes("filter-branch") || args.includes("filter-repo")) return true;
  if (args.includes("difftool") && args.some((value) => value.startsWith("-x") || value.startsWith("--ext"))) return true;
  return (args.includes("submodule") && args.includes("foreach")) || (args.includes("bisect") && args.includes("run"));
}

// The sed `e` command and the s///e flag run their text as a shell command line. Each command of a script is read
// in turn: addresses and the bodies of s and y (delimited text) are skipped, text commands (a, i, c, r, w, #) run to
// the end of their line, and an `e` command or an `e` flag on s fails closed. GNU sed only: BSD sed has no `e`.
function sedScriptRunsShell(script) {
  const n = script.length;
  let i = 0;
  const skipDelimited = (delimiter) => {
    while (i < n && script[i] !== delimiter) i += script[i] === "\\" ? 2 : 1;
    i += 1;
  };
  const skipOneAddress = () => {
    while (i < n && /[\d\s$+~!]/.test(script[i])) i += 1;
    if (script[i] === "/") {
      i += 1;
      skipDelimited("/");
      while (i < n && /[IM]/.test(script[i])) i += 1;
    } else if (script[i] === "\\") {
      const delimiter = script[i + 1];
      i += 2;
      skipDelimited(delimiter);
      while (i < n && /[IM]/.test(script[i])) i += 1;
    }
  };
  // An address may be a range, addr1,addr2, and each end may be a regex, a line number, $, or +N / ~N.
  const skipAddress = () => {
    skipOneAddress();
    while (i < n && /\s/.test(script[i])) i += 1;
    if (script[i] === ",") {
      i += 1;
      skipOneAddress();
    }
  };
  while (i < n) {
    while (i < n && /[\s;{}\n]/.test(script[i])) i += 1;
    if (i >= n) break;
    skipAddress();
    while (i < n && /[\s!{]/.test(script[i])) i += 1;
    const command = script[i];
    if (command === undefined) break;
    i += 1;
    if (command === "e") return true;
    if (command === "s") {
      const delimiter = script[i];
      i += 1;
      skipDelimited(delimiter);
      skipDelimited(delimiter);
      while (i < n && !/[;\n}]/.test(script[i])) {
        if (script[i] === "e") return true;
        i += 1;
      }
    } else if (command === "y") {
      const delimiter = script[i];
      i += 1;
      skipDelimited(delimiter);
      skipDelimited(delimiter);
    } else if ("aicrwRW#:".includes(command)) {
      while (i < n && script[i] !== "\n") i += 1;
    } else {
      while (i < n && !/[;\n]/.test(script[i])) i += 1;
    }
  }
  return false;
}

// sed runs the text of an `e` command or an s///e flag as a shell command line. A script file, and an option
// cluster that could take an argument, cannot be read statically here and fail closed.
function sedRunsScript(args) {
  const scripts = [];
  const operands = [];
  for (let i = 0; i < args.length; i += 1) {
    const value = args[i];
    if (value === "--") {
      operands.push(...args.slice(i + 1));
      break;
    }
    if (value === "-e" || value === "--expression") {
      scripts.push(args[i + 1] ?? "");
      i += 1;
    } else if (value.startsWith("--expression=")) {
      scripts.push(value.slice("--expression=".length));
    } else if (value === "--line-length") {
      i += 1;
    } else if (value.startsWith("--file") || (/^-[A-Za-z]/.test(value) && /[efl]/.test(value.slice(1)))) {
      return true;
    } else if (!value.startsWith("-") || value === "-") {
      operands.push(value);
    }
  }
  // With -e or --expression the operands are files, but a static reader cannot pair each word with its option
  // across GNU getopt permutation and BSD, so every operand is also read as script text.
  const script = scripts.length > 0 ? [...scripts, ...operands] : operands.slice(0, 1);
  return script.some((text) => sedScriptRunsShell(text));
}

// Whether xargs launches its replacement string as the command (xargs -I{} {}), or passes it as the program
// text of a shell or eval. The launched command then comes from upstream output, which cannot be read here.
function xargsRunsReplacement(position) {
  if (!position.wrappers.includes("xargs") || !position.command) return false;
  const replacements = new Set(["{}"]);
  const before = position.words.slice(0, position.index).map((word) => word.value);
  for (let i = 0; i < before.length; i += 1) {
    if (before[i] === "-I" && before[i + 1]) replacements.add(before[i + 1]);
    else if (before[i].startsWith("-I") && before[i].length > 2) replacements.add(before[i].slice(2));
    else if (before[i].startsWith("--replace=")) replacements.add(before[i].slice("--replace=".length));
  }
  const command = position.command.value;
  if (replacements.has(command)) return true;
  if (["eval", ".", "source"].includes(command)) return position.words.slice(position.index + 1).some((word) => replacements.has(word.value));
  const shell = shellInvocation(position);
  return Boolean(shell && shell.kind !== "stdin" && shell.payload && replacements.has(shell.payload.value));
}

// Whether xargs reads its arguments from an -a / --arg-file file, which this policy cannot see. Short option
// clusters are walked the same way xargs reads them: an `a` before any option that takes an argument.
function xargsReadsArgFile(position) {
  return position.words.slice(0, position.index).some((word) => {
    const value = word.value;
    if (value.startsWith("--")) return resolveXargsLongOption(value.slice(2).split("=")[0]) === "arg-file";
    if (!value.startsWith("-") || value === "-") return false;
    for (const letter of value.slice(1)) {
      if (letter === "a") return true;
      if (XARGS_TAKES_ARGUMENT.has(letter)) return false;
    }
    return false;
  });
}

// Whether xargs reads its arguments from stdin or an arg file that this policy cannot see: a file redirect,
// an -a file, or an upstream command other than a literal echo or printf. Such a replacement command is unknown,
// so it fails closed.
function xargsStdinUnknown(program, tokens, position) {
  if (xargsReadsArgFile(position)) return true;
  if (tokens.some((token) => token.type === "redir" && token.fd === 0 && token.value === "<")) return true;
  const piped = program.separators.some((sep) => sep === "|" || sep === "|&");
  return piped && program.nodes.some((node) => {
    if (node === tokens) return false;
    const first = wordsInNode(node)[0];
    return !first || !["echo", "printf"].includes(basename(first.value));
  });
}

// Text of every word, substitution, heredoc body, and group in one node. A watcher name can reach the
// executed command through any of them, so all of them are scanned.
function nodeTextFragments(tokens) {
  const fragments = [];
  for (const token of tokens) {
    if (token.type === "word") {
      fragments.push(token.value);
      for (const substitution of token.subs) fragments.push(substitution.content);
    } else if (token.type === "redir" && typeof token.heredoc === "string") {
      fragments.push(token.heredoc);
    } else if (token.type === "group") {
      fragments.push(token.content);
    }
  }
  return fragments;
}

// Quote and backslash bytes are dropped as the transport prefilter drops them, so a split name such as
// fm-"watch"-arm.sh still matches. A quoting-decoder marker ($' or $") can hide a name, so it counts.
function mentionsWatcherText(fragment) {
  if (/\$['"]/.test(fragment)) return true;
  const stripped = normalizeLineContinuations(fragment).replace(/[\\"'\r\n]/g, "");
  return /fm-watch/i.test(stripped) || DYNAMIC_WATCHER_NAME.test(stripped) || DYNAMIC_WATCHER_NAME.test(expansionSkeleton(stripped));
}

// A reference to a variable that already holds a watcher name or pattern, such as $W.
function textReferencesAny(text, names) {
  if (names.size === 0) return false;
  for (const match of text.matchAll(/\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))/g)) {
    if (names.has(match[1] || match[2])) return true;
  }
  return false;
}

// Whether any text of a node names a watcher, directly or through a variable that holds one.
function watcherMentionInNode(tokens, context) {
  const names = new Set([...(context.protectedVariables || []), ...(context.watcherPatterns || [])]);
  return nodeTextFragments(tokens).some((fragment) => mentionsWatcherText(fragment) || textReferencesAny(fragment, names));
}

// watch runs its operand words as one shell command line (sh -c "$*"). Its interval option takes an
// argument; an option this table does not list leaves the operand unresolved, which fails closed when a
// protected name is present.
const WATCH_NO_ARGUMENT = /^-[dtbegcCxpwrqv]+$|^--(differences|beep|errexit|color|no-title|precise|chgexit|shotsdir|no-wrap|exec|no-color)(=.*)?$/;
function watchOperands(position) {
  const none = { payloads: [], unresolved: false };
  if (!position.command || basename(position.command.value) !== "watch") return none;
  const words = position.words;
  let index = position.index + 1;
  while (index < words.length) {
    const value = words[index].value;
    if (value === "--") {
      index += 1;
      break;
    }
    if (value === "-n" || value === "--interval") {
      index += 2;
      continue;
    }
    if (/^-n./.test(value) || /^--interval=/.test(value) || WATCH_NO_ARGUMENT.test(value)) {
      index += 1;
      continue;
    }
    if (value.startsWith("-") && value !== "-") return { payloads: [], unresolved: true };
    break;
  }
  const operand = words.slice(index).map((word) => word.value).join(" ");
  return { payloads: operand ? [operand] : [], unresolved: false };
}

function matchingFzfActionEnd(value, open) {
  let depth = 1;
  let quote = "";
  let escaped = false;
  for (let index = open + 1; index < value.length; index += 1) {
    const char = value[index];
    if (escaped) {
      escaped = false;
    } else if (char === "\\") {
      escaped = true;
    } else if (quote) {
      if (char === quote) quote = "";
    } else if (char === "'" || char === '"') {
      quote = char;
    } else if (char === "(") {
      depth += 1;
    } else if (char === ")" && --depth === 0) {
      return index;
    }
  }
  return -1;
}

// Closing delimiter for each opening bracket an fzf action accepts (fzf(1): `execute(...)`, `execute[...]`,
// `execute<...>`, `execute{...}`). Any other punctuation closes on itself (`execute~...~`, `execute!...!`), and
// `:` is the open-ended form that runs to the end of the binding.
const FZF_ACTION_CLOSERS = { "(": ")", "[": "]", "<": ">", "{": "}" };

// Every action argument is classified as shell text, whatever the action is. The executing actions change across
// fzf releases (checked against the fzf(1) pages for 0.24.3, 0.35.0, 0.44.1, and v0.74.5: preview, transform-*,
// bg-transform-*, and others), so a name list would go stale. An action name is a word after `:` (the key separator)
// or `+` (the action separator). A name followed by a space, `+`, or `,` has no argument.
function fzfBindingPayloads(binding) {
  const payloads = [];
  let unresolved = false;
  for (const match of binding.matchAll(/(?<=[:+])([A-Za-z][A-Za-z-]*)/g)) {
    const open = match.index + match[1].length;
    const delimiter = binding[open];
    if (delimiter === undefined || /[\s+,]/.test(delimiter)) continue;
    if (delimiter === ":") {
      // The open-ended form takes the rest of the binding, so a protected name anywhere after it denies.
      payloads.push(binding.slice(open + 1));
      break;
    }
    const close = delimiter === "("
      ? matchingFzfActionEnd(binding, open)
      : binding.indexOf(FZF_ACTION_CLOSERS[delimiter] ?? delimiter, open + 1);
    if (close === -1) {
      unresolved = true;
      continue;
    }
    payloads.push(binding.slice(open + 1, close));
  }
  return { payloads, unresolved };
}

function fzfOperands(position) {
  const none = { payloads: [], unresolved: false };
  if (!position.command || basename(position.command.value) !== "fzf") return none;
  const words = position.words;
  const payloads = [];
  let unresolved = false;
  for (let index = position.index + 1; index < words.length; index += 1) {
    const value = words[index].value;
    if (value === "--preview" || value === "--bind" || value === "-b") {
      const operand = words[index + 1]?.value;
      if (operand === undefined) {
        unresolved = true;
        continue;
      }
      if (value === "--preview") payloads.push(operand);
      else {
        const bindings = fzfBindingPayloads(operand);
        payloads.push(...bindings.payloads);
        unresolved ||= bindings.unresolved;
      }
      index += 1;
      continue;
    }
    if (value.startsWith("--preview=")) {
      payloads.push(value.slice("--preview=".length));
    } else if (value.startsWith("--bind=")) {
      const bindings = fzfBindingPayloads(value.slice("--bind=".length));
      payloads.push(...bindings.payloads);
      unresolved ||= bindings.unresolved;
    } else if (value.startsWith("-b") && value.length > 2) {
      const bindings = fzfBindingPayloads(value.slice(2));
      payloads.push(...bindings.payloads);
      unresolved ||= bindings.unresolved;
    }
  }
  return { payloads, unresolved };
}

// After -c the shell still reads options (-e, -s, -l, -o NAME) up to `--`. The first non-option word is the
// command string, and the words after it are $0 and the positional parameters.
function shellCommandPayload(words, start) {
  let index = start;
  let optionsEnded = false;
  while (index < words.length) {
    const value = words[index].value;
    if (!optionsEnded && value === "--") {
      optionsEnded = true;
      index += 1;
    } else if (!optionsEnded && /^[-+]/.test(value) && value !== "-") {
      index += shellOptionWord(value)?.takesNext ? 2 : 1;
    } else {
      break;
    }
  }
  return { word: words[index] || null, operands: words.slice(index + 1) };
}

function shellInvocation(position) {
  if (!position.command) return null;
  const name = basename(position.command.value);
  if (!SHELL_NAMES.has(name)) return null;
  const words = position.words;
  let readsStdin = false;
  let optionsEnded = false;
  for (let i = position.index + 1; i < words.length; i += 1) {
    const option = words[i];
    const shellOption = shellOptionWord(option.value);
    if (shellOption?.command) {
      const { word, operands } = shellCommandPayload(words, i + 1 + (shellOption.takesNext ? 1 : 0));
      return { kind: "command", payload: word, operands };
    }
    if (shellOption?.takesNext && !optionsEnded) {
      i += 1;
      continue;
    }
    if (!optionsEnded && /^-[A-Za-z]*s[A-Za-z]*$/.test(option.value)) readsStdin = true;
    if (option.value === "--") {
      // `--` ends option parsing: after -s a later `-c` is only a positional
      // parameter, and a later `-s` never switches to reading stdin.
      if (readsStdin) return { kind: "stdin", payload: null, operand: words[i + 1] || null };
      optionsEnded = true;
    }
    if (option.value === "--" || /^[-+]/.test(option.value)) continue;
    // With -s the shell still reads its program from stdin; the operand is only
    // a positional parameter, kept as `operand` so a protected path there still
    // fails closed.
    if (readsStdin) return { kind: "stdin", payload: null, operand: option };
    return { kind: "script", payload: option };
  }
  return { kind: "stdin", payload: null };
}

function shellHeredocPayloads(tokens, position) {
  if (shellInvocation(position)?.kind !== "stdin") return [];
  const heredocs = tokens.filter((token) => token.type === "redir" && token.fd === 0 && typeof token.heredoc === "string");
  return heredocs.length === 0 ? [] : [heredocs.at(-1).heredoc];
}

function shellHereStringPayloads(tokens, position) {
  if (shellInvocation(position)?.kind !== "stdin") return [];
  const payloads = [];
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token.type !== "redir" || token.value !== "<<<" || token.fd !== 0) continue;
    const payload = tokens[i + 1];
    if (payload?.type === "word" && payload.literal && payload.subs.length === 0) payloads.push(payload.value);
  }
  return payloads;
}

function sourcedScript(position) {
  if (!position.command || ![".", "source"].includes(position.command.value)) return null;
  return position.words[position.index + 1] || null;
}

// The text an eval runs: literal words as written, and other words once their positional or line-local
// values are known. Null when any word is still unknown, which the caller treats as dynamic.
function evalPayload(position, assigned) {
  if (!position.command || basename(position.command.value) !== "eval") return null;
  const payloads = position.words.slice(position.index + 1);
  if (payloads.length === 0 || payloads.some((payload) => payload.subs.length > 0)) return null;
  const texts = [];
  for (const payload of payloads) {
    if (payload.literal) {
      texts.push(payload.value);
      continue;
    }
    const expansion = expandWord(payload.value, assigned);
    if (expansion.dynamic) return null;
    texts.push(expansion.text);
  }
  return texts.join(" ");
}

function wordHasUnknownPositional(word, assigned) {
  return Boolean(word && word.subs.length === 0 && expandWord(word.value, assigned).unknownPositional);
}

function hasUnknownPositionalEval(position, assigned) {
  if (!position.command || basename(position.command.value) !== "eval") return false;
  return position.words.slice(position.index + 1).some((payload) => wordHasUnknownPositional(payload, assigned));
}

// The text a word expands to when every part is known, or null when any part is unknown.
function knownWordText(word, assigned) {
  if (!word || word.subs.length > 0) return null;
  const expansion = expandWord(word.value, assigned);
  return expansion.dynamic ? null : expansion.text;
}

// Whether a word used as a sourced or executed script names a watcher once its positional values are known.
function positionalWordMayNameWatcher(word, assigned, root) {
  if (!word || word.subs.length > 0) return false;
  const expansion = expandWord(word.value, assigned);
  if (expansion.unknownPositional) return true;
  return !expansion.dynamic && Boolean(protectedIdentity(expansion.text, root));
}

// The operands of a shell -c are its $0 and then its positional parameters, which set -- and shift can change.
function withShellPositionals(context, operands) {
  const assigned = new Map(context.assigned || []);
  assigned.set(POSITIONALS, operands.slice(1).map((word) => (word.literal && word.subs.length === 0 ? word.value : null)));
  return { ...context, assigned };
}

// set -- and shift change the positional parameters that later words of the same line expand. A set whose
// words are all options leaves them unchanged. A non-literal word or an unknown shift count makes them unknown.
function applyPositionalBuiltin(position, context) {
  if (!position.command) return context;
  const name = basename(position.command.value);
  if (name !== "set" && name !== "shift") return context;
  const args = position.words.slice(position.index + 1);
  const current = context.assigned.get(POSITIONALS);
  let next;
  if (name === "shift") {
    if (current === undefined) return context;
    const count = args.length === 0 ? 1 : args.length === 1 && args[0].literal && /^\d+$/.test(args[0].value) ? Number(args[0].value) : NaN;
    next = current === null || Number.isNaN(count) || count > current.length ? null : current.slice(count);
  } else {
    let index = 0;
    let endOfOptions = false;
    while (index < args.length && /^[-+]./.test(args[index].value)) {
      if (args[index].value === "--") {
        endOfOptions = true;
        index += 1;
        break;
      }
      index += /^[-+][A-Za-z]*[oO][A-Za-z]*$/.test(args[index].value) ? 2 : 1;
    }
    if (!endOfOptions && index >= args.length) return context;
    next = args.slice(index).map((word) => (word.literal && word.subs.length === 0 ? word.value : null));
  }
  const assigned = new Map(context.assigned);
  assigned.set(POSITIONALS, next);
  return { ...context, assigned };
}


function wordReferencesAny(word, names) {
  if (!word || names.size === 0) return false;
  for (const match of word.value.matchAll(/\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))/g)) {
    if (names.has(match[1] || match[2])) return true;
  }
  return false;
}

function hasDynamicExecutionPayload(position, context) {
  if (!position.command) return false;
  const name = basename(position.command.value);
  const shell = shellInvocation(position);
  if (shell?.kind === "command") {
    const payload = shell.payload;
    return Boolean(payload && (!payload.literal || payload.subs.length > 0) && wordReferencesAny(payload, context.protectedVariables));
  }
  if (name === "eval") {
    return position.words.slice(position.index + 1).some((payload) => (!payload.literal || payload.subs.length > 0) && wordReferencesAny(payload, context.protectedVariables));
  }
  return false;
}

function assignmentName(word) {
  const match = word.value.match(/^([A-Za-z_][A-Za-z0-9_]*)=/);
  return match ? match[1] : "";
}

function assignmentHasUnknownPositional(value, assigned) {
  const list = assigned.get(POSITIONALS);
  const parameter = /\$\{(\d+|[@*])([^}]*)\}|\$(\d+|[@*])/g;
  for (const match of value.matchAll(parameter)) {
    const ref = match[1] || match[3];
    if (match[2] || ref === "@" || ref === "*") return true;
    if (list === undefined || list === null || list[Number(ref) - 1] === null) return true;
  }
  return false;
}

function contextWithAssignments(context, words, inNested = false) {
  const protectedVariables = new Set(context.protectedVariables || []);
  const watcherPatterns = new Set(context.watcherPatterns || []);
  const watcherPids = new Set(context.watcherPids || []);
  const assigned = new Map(context.assigned || []);
  const unknownPositionalNames = new Set(assigned.get(UNKNOWN_POSITIONAL_NAMES) || []);
  for (const word of words) {
    const name = assignmentName(word);
    if (!name) continue;
    if (inNested) context.nestedNames?.add(name);
    const value = word.value.slice(word.value.indexOf("=") + 1);
    const expandedValue = word.subs.length === 0 ? expandWord(value, assigned) : null;
    const expandsToWatcher = expandedValue && !expandedValue.dynamic && mentionsWatcherText(expandedValue.text);
    if (rawMentionsProtected(value) || expandsToWatcher || wordReferencesAny(word, protectedVariables)) protectedVariables.add(name);
    else protectedVariables.delete(name);
    if (/fm-watch/i.test(value) || wordReferencesAny(word, watcherPatterns)) watcherPatterns.add(name);
    else watcherPatterns.delete(name);
    if (wordReferencesAny(word, watcherPids)) watcherPids.add(name);
    else watcherPids.delete(name);
    if (assignmentHasUnknownPositional(value, assigned) || expandedValue?.unknownPositional || GLOB_OR_BRACE.test(value) || wordReferencesAny(word, unknownPositionalNames)) {
      unknownPositionalNames.add(name);
    } else {
      unknownPositionalNames.delete(name);
    }
    // A literal assignment resolves later expansions in the same command line; a command
    // substitution value is unknowable, so the name is recorded as opaque (null).
    if (word.subs.length > 0) assigned.set(name, null);
    else assigned.set(name, value);
  }
  assigned.set(UNKNOWN_POSITIONAL_NAMES, unknownPositionalNames);
  return { ...context, protectedVariables, watcherPatterns, watcherPids, assigned };
}

function nodeHasRedirection(tokens) {
  return tokens.some((token) => token.type === "redir");
}

function nodeHasUnsafeSubstitution(tokens) {
  return tokens.some((token) => token.type === "word" && token.subs.length > 0);
}

function isWatcherPgrep(position, context) {
  if (!position.command || basename(position.command.value) !== "pgrep") return false;
  return position.words.slice(position.index + 1).some((word) => wordNamesWatcher(word, context));
}

// nice, stdbuf, caffeinate, and builtin run their operand as a command, and a leading `!` negates
// it. They are skipped with their options before the command word is chosen, so a protected name
// behind them keeps its identity. An option the table does not list is left unresolved, which fails
// closed when the command line also mentions a protected watcher.
const ARM_WRAPPERS = new Set(["nice", "stdbuf", "caffeinate", "builtin"]);

function armWrapperOptionWidth(name, option) {
  if (name === "nice") {
    if (option === "-n" || option === "--adjustment") return 2;
    if (/^-\d+$/.test(option) || /^-n./.test(option) || /^--adjustment=/.test(option)) return 1;
    return 0;
  }
  if (name === "stdbuf") {
    if (/^-[ioe]$/.test(option) || option === "--input" || option === "--output" || option === "--error") return 2;
    if (/^-[ioe]./.test(option) || /^--(input|output|error)=/.test(option)) return 1;
    return 0;
  }
  if (name === "caffeinate") {
    if (option === "-t" || option === "-w") return 2;
    if (/^-[dimsu]+$/.test(option)) return 1;
    return 0;
  }
  return 0;
}

function armCommandPosition(tokens) {
  const words = wordsInNode(tokens);
  const dropped = new Set();
  const wrappers = [];
  let unresolved = false;
  let index = 0;
  while (index < words.length && !unresolved) {
    const value = words[index].value;
    if (value === "!") {
      wrappers.push("!");
      dropped.add(words[index]);
      index += 1;
      continue;
    }
    const name = basename(value);
    if (!ARM_WRAPPERS.has(name)) break;
    wrappers.push(name);
    dropped.add(words[index]);
    index += 1;
    while (index < words.length && words[index].value.startsWith("-") && words[index].value !== "-") {
      const width = armWrapperOptionWidth(name, words[index].value);
      if (width === 0) {
        unresolved = true;
        break;
      }
      for (let step = 0; step < width && index < words.length; step += 1) {
        dropped.add(words[index]);
        index += 1;
      }
    }
  }
  const position = commandPosition(tokens.filter((token) => !dropped.has(token)));
  position.wrappers = [...wrappers, ...position.wrappers];
  if (unresolved) position.unresolvedWrapperOption = true;
  return position;
}

function analyzeProgram(command, context, depth = 0) {
  if (depth > 12) {
    return { error: "recursion limit", protectedFound: rawMentionsProtected(command), broadKill: rawMentionsBroadKill(command), pgrepWatcher: false, watcherPids: new Set() };
  }
  const lexed = new Lexer(command).tokenize();
  if (lexed.error) {
    return { error: lexed.error, protectedFound: rawMentionsProtected(command), broadKill: rawMentionsBroadKill(command), pgrepWatcher: false, watcherPids: new Set() };
  }
  const program = splitProgram(lexed.tokens);
  const nodeInfos = [];
  let nestedProtected = false;
  let broadKill = false;
  let pgrepWatcher = false;
  let unsupported = false;
  let activeContext = {
    ...context,
    protectedVariables: new Set(context.protectedVariables || []),
    watcherPatterns: new Set(context.watcherPatterns || []),
    watcherPids: new Set(context.watcherPids || []),
    assigned: new Map(context.assigned || []),
  };
  let unclassifiableProtected = false;
  const programMentionsWatcher = program.nodes.some((tokens) => watcherMentionInNode(tokens, context));

  for (const tokens of program.nodes) {
    const position = armCommandPosition(tokens);
    const nodeContext = contextWithAssignments(activeContext, position.words, depth > 0);
    const firstName = basename(position.words[0]?.value || "");
    if (["if", "then", "else", "elif", "fi", "for", "while", "until", "case", "esac", "do", "done", "function", "time", "coproc"].includes(firstName)) {
      unsupported = true;
    }

    let nodeNestedProtected = false;
    let nodePgrepWatcher = false;
    const substitutionResults = new Map();
    const watchOperand = watchOperands(position);
    if (watchOperand.unresolved) position.unresolvedWrapperOption = true;
    const fzfOperand = fzfOperands(position);
    if (fzfOperand.unresolved && watcherMentionInNode(tokens, nodeContext)) unclassifiableProtected = true;
    if (runsOperandCommand(position) && watcherMentionInNode(tokens, nodeContext)) nodeNestedProtected = true;
    for (const payload of [...position.wrapperPayloads, ...watchOperand.payloads, ...fzfOperand.payloads]) {
      const nested = analyzeProgram(payload, nodeContext, depth + 1);
      nodeNestedProtected ||= nested.protectedFound;
      broadKill ||= nested.broadKill;
      nodePgrepWatcher ||= nested.pgrepWatcher;
      if (nested.error && rawMentionsProtected(payload)) unsupported = true;
    }
    for (const token of tokens) {
      if (token.type === "group") {
        const nested = analyzeProgram(token.content, nodeContext, depth + 1);
        nodeNestedProtected ||= nested.protectedFound;
        broadKill ||= nested.broadKill;
        nodePgrepWatcher ||= nested.pgrepWatcher;
        if (nested.error && rawMentionsProtected(token.content)) unsupported = true;
      }
      if (token.type === "word") {
        for (const substitution of token.subs) {
          const nested = analyzeProgram(substitution.content, nodeContext, depth + 1);
          substitutionResults.set(substitution, nested);
          nodeNestedProtected ||= nested.protectedFound;
          broadKill ||= nested.broadKill;
          nodePgrepWatcher ||= nested.pgrepWatcher;
          if (nested.error && rawMentionsProtected(substitution.content)) unsupported = true;
        }
      }
    }

    const shell = shellInvocation(position);
    const shellContext = shell?.kind === "command" ? withShellPositionals(nodeContext, shell.operands) : nodeContext;
    const shellPayload = shell?.kind === "command" ? shell.payload : null;
    const shellScript = shell?.kind === "script" ? shell.payload : shell?.operand || null;
    const sourceScript = sourcedScript(position);
    const literalEvalPayload = evalPayload(position, nodeContext.assigned);
    const heredocPayloads = shellHeredocPayloads(tokens, position);
    const hereStringPayloads = shellHereStringPayloads(tokens, position);
    const sourcesProcess = Boolean(position.command) && [".", "source"].includes(position.command.value) && tokens.some((token) => token.type === "group");
    if (sourceScript?.subs.length > 0 || sourcesProcess) unclassifiableProtected = true;
    for (const script of [shellScript, sourceScript]) {
      if (!script) continue;
      nodeNestedProtected ||= Boolean(protectedIdentity(script.value, context.root)) || wordReferencesAny(script, nodeContext.protectedVariables);
      nodeNestedProtected ||= positionalWordMayNameWatcher(script, nodeContext.assigned, context.root);
      unclassifiableProtected ||= hasUnclassifiableProtectedExpansion(script, context.root);
    }
    if (shellPayload && (!shellPayload.literal || shellPayload.subs.length > 0)) {
      if (wordReferencesAny(shellPayload, nodeContext.protectedVariables)) nodeNestedProtected = true;
      if (wordHasUnknownPositional(shellPayload, nodeContext.assigned)) nodeNestedProtected = true;
      const knownPayload = knownWordText(shellPayload, nodeContext.assigned);
      if (knownPayload !== null) {
        const nested = analyzeProgram(knownPayload, shellContext, depth + 1);
        nodeNestedProtected ||= nested.protectedFound;
        broadKill ||= nested.broadKill;
        nodePgrepWatcher ||= nested.pgrepWatcher;
        if (nested.error && rawMentionsProtected(knownPayload)) unsupported = true;
      }
      // The inner shell decodes $'...' itself, so a payload that contains one is read as shell text.
      if (shellPayload.subs.length === 0 && /\$['"]/.test(shellPayload.value)) {
        const nested = analyzeProgram(shellPayload.value, shellContext, depth + 1);
        nodeNestedProtected ||= nested.protectedFound;
        broadKill ||= nested.broadKill;
        nodePgrepWatcher ||= nested.pgrepWatcher;
        if (nested.error && rawMentionsProtected(shellPayload.value)) unsupported = true;
      }
    } else if (shellPayload) {
      const nested = analyzeProgram(shellPayload.value, shellContext, depth + 1);
      nodeNestedProtected ||= nested.protectedFound;
      broadKill ||= nested.broadKill;
      nodePgrepWatcher ||= nested.pgrepWatcher;
      if (nested.error && rawMentionsProtected(shellPayload.value)) unsupported = true;
    }
    // $0 and the positional parameters of a shell -c are operands the command string can run ("$@", $0, eval "$1").
    if (shell?.kind === "command" && shell.operands.some((word) => mentionsWatcherText(word.value) || wordReferencesAny(word, nodeContext.protectedVariables))) {
      nodeNestedProtected = true;
    }
    // A shell whose program text comes from a command or process substitution, from xargs, from a
    // pipe, or from a redirected file cannot be read statically, so that launch fails closed.
    if (shell) {
      const substitutedProgram = Boolean(shellPayload && shellPayload.subs.length > 0) || Boolean(shell.kind === "script" && shell.payload && shell.payload.subs.length > 0);
      const stdinUnknown = shell.kind === "stdin" && heredocPayloads.length === 0 && hereStringPayloads.length === 0 && (program.separators.some((sep) => sep === "|" || sep === "|&") || tokens.some((token) => token.type === "redir" && token.fd === 0 && token.value === "<"));
      if (position.wrappers.includes("xargs") || substitutedProgram || stdinUnknown) unclassifiableProtected = true;
    }
    if (programMentionsWatcher && xargsRunsReplacement(position)) unclassifiableProtected = true;
    // An editor reads commands from stdin, so what a pipe or redirect feeds it runs. A pipeline that mentions a
    // watcher denies; a redirected file has unknown content and fails closed.
    if (position.command && EDITOR_EXECUTORS.test(basename(position.command.value))) {
      if (program.separators.some((sep) => sep === "|" || sep === "|&") && programMentionsWatcher) nodeNestedProtected = true;
      if (tokens.some((token) => token.type === "redir" && token.fd === 0 && token.value === "<")) unclassifiableProtected = true;
    }
    if (xargsRunsReplacement(position) && xargsStdinUnknown(program, tokens, position)) unclassifiableProtected = true;
    for (const payload of [literalEvalPayload, ...heredocPayloads, ...hereStringPayloads]) {
      if (payload === null) continue;
      const nested = analyzeProgram(payload, nodeContext, depth + 1);
      nodeNestedProtected ||= nested.protectedFound;
      broadKill ||= nested.broadKill;
      nodePgrepWatcher ||= nested.pgrepWatcher;
      if (nested.error && rawMentionsProtected(payload)) unsupported = true;
    }
    if (hasUnknownPositionalEval(position, nodeContext.assigned)) nodeNestedProtected = true;

    // A name assigned inside a nested payload (eval, group, shell -c) may be set by the time this
    // node runs, so it has no known literal value from here on.
    for (const name of nodeContext.nestedNames || []) nodeContext.assigned.set(name, null);
    // A reserved word such as do or then leads a compound body; the command it introduces is the
    // next word, so that word gets the same command-position check.
    const leadingKeywords = new Set(["do", "then", "else", "elif", "if", "while", "until", "time", "!"]);
    let keywordCount = 0;
    while (keywordCount < position.words.length && leadingKeywords.has(position.words[keywordCount].value)) keywordCount += 1;
    if (keywordCount > 0 && position.words[keywordCount] && wordCouldNameProtectedScript(position.words[keywordCount], nodeContext.assigned, context.root)) unclassifiableProtected = true;
    const executable = position.command?.value || "";
    // A command word whose variables all resolve to literals is classified by its resolved name.
    const resolvedCommand = position.command && position.command.subs.length === 0 ? expandWord(executable, nodeContext.assigned) : null;
    const protectedKind = protectedIdentity(resolvedCommand && !resolvedCommand.dynamic ? resolvedCommand.text : executable, context.root);
    // A command word that is an expansion and not a protected path ($SHELL, ${RUNNER}) names its program only at run time.
    if (executable.includes("$") && !protectedKind && watcherMentionInNode(tokens, nodeContext)) nodeNestedProtected = true;
    if (hasUnclassifiableProtectedExpansion(position.command, context.root)) unclassifiableProtected = true;
    if (wordCouldNameProtectedScript(position.command, nodeContext.assigned, context.root)) unclassifiableProtected = true;
    const commandName = basename(executable);
    const args = position.words.slice(position.index + 1);
    if (["pkill", "killall", "fuser"].includes(commandName) && args.some((word) => wordNamesWatcher(word, nodeContext))) broadKill = true;
    // A kill-family command whose substitution text names the watcher targets it, as in
    // kill -9 $(ps -ef | grep fm-watch | awk '{print $2}').
    if (KILL_COMMANDS.has(commandName) && tokens.some((token) => token.type === "word" && token.subs.some((substitution) => /fm-watch/i.test(substitution.content)))) broadKill = true;
    // A kill after a watcher pgrep in the same command line, including `pgrep ... | xargs kill`, targets the watcher.
    if (commandName === "kill" && (pgrepWatcher || nodePgrepWatcher || args.some((word) => wordReferencesAny(word, nodeContext.watcherPids)))) broadKill = true;
    if (isWatcherPgrep(position, nodeContext)) pgrepWatcher = true;
    if (hasDynamicExecutionPayload(position, nodeContext) || wordReferencesAny(position.command, nodeContext.protectedVariables)) nodeNestedProtected = true;
    for (const word of position.words) {
      const name = assignmentName(word);
      if (!name) continue;
      if (word.subs.some((substitution) => substitutionResults.get(substitution)?.pgrepWatcher)) nodeContext.watcherPids.add(name);
    }
    pgrepWatcher ||= nodePgrepWatcher;
    nestedProtected ||= nodeNestedProtected;
    activeContext = applyPositionalBuiltin(position, nodeContext);
    if (position.unresolvedWrapperOption) unsupported = true;
    nodeInfos.push({
      tokens,
      position,
      commandName,
      context: nodeContext,
      protectedKind,
      nestedProtected: nodeNestedProtected,
      redirection: nodeHasRedirection(tokens),
      substitution: nodeHasUnsafeSubstitution(tokens),
    });
  }

  // A kill-family command in the same pipeline as a selector that names the watcher, such as
  // ps aux | grep fm-watch.sh | xargs kill, targets the watcher.
  for (const group of pipelineGroups(nodeInfos, program.separators)) {
    const killing = group.some((info) => KILL_COMMANDS.has(info.commandName));
    const selecting = group.some((info) => SELECTOR_COMMANDS.has(info.commandName) && info.position.words.slice(info.position.index + 1).some((word) => wordNamesWatcher(word, info.context)));
    if (killing && selecting) broadKill = true;
  }
  const directProtected = nodeInfos.some((info) => Boolean(info.protectedKind));
  const protectedFound = directProtected || nestedProtected || unclassifiableProtected;
  if (unclassifiableProtected) unsupported = true;
  const broadKillFound = broadKill || (unsupported && rawMentionsBroadKill(command));
  if (unsupported && (protectedFound || rawMentionsProtected(command) || broadKillFound)) {
    return { error: "unsupported compound grammar", protectedFound: true, broadKill: broadKillFound, pgrepWatcher, watcherPids: activeContext.watcherPids, program, nodeInfos };
  }
  return { error: "", protectedFound, directProtected, nestedProtected, broadKill: broadKillFound, pgrepWatcher, watcherPids: activeContext.watcherPids, program, nodeInfos };
}

function ordinaryWordsOnly(tokens) {
  return tokens.every((token) => token.type === "word" && token.subs.length === 0);
}

function setupKind(info, context) {
  const { tokens, position } = info;
  if (!ordinaryWordsOnly(tokens) || position.prefixAssignments > 0 || position.wrappers.length > 0) return "";
  const values = position.words.map((word) => word.value);
  if (values[0] === "cd" && values.length === 2) return "cd";
  if (values[0] === "export" && values.length === 2 && isAssignment(values[1]) && !UNSAFE_SETUP_VARIABLE.test(values[1].slice(0, values[1].indexOf("=")))) return "export";
  return "";
}

function finalProtectedAllowed(info) {
  if (!info.protectedKind || !FINAL_ALLOWED_KINDS.has(info.protectedKind) || info.redirection || info.substitution) return false;
  if (!ordinaryWordsOnly(info.tokens) || info.position.prefixAssignments > 0) return false;
  const wrappers = info.position.wrappers;
  return wrappers.length === 0 || (wrappers.length === 1 && wrappers[0] === "exec");
}

function blessedProgram(analysis, context) {
  const { nodeInfos } = analysis;
  const separators = analysis.program.separators;
  if (nodeInfos.length === 0 || separators.some((separator) => ![";", "newline", "&&"].includes(separator))) return false;
  if (!finalProtectedAllowed(nodeInfos.at(-1))) return false;
  if (nodeInfos.slice(0, -1).some((info) => info.protectedKind || info.nestedProtected)) return false;

  const setup = nodeInfos.slice(0, -1).map((info) => setupKind(info, context));
  if (setup.some((kind) => !kind)) return false;
  // A setup `cd` changes what a relative protected path resolves to, so with a relative final
  // path the cd target must be the code root itself.
  const finalCommand = nodeInfos.at(-1).position.command?.value || "";
  if (isRelativePath(finalCommand)) {
    const root = trimTrailingSlashes(context.root);
    const cdOutsideRoot = nodeInfos.slice(0, -1).some((info, index) => setup[index] === "cd" && trimTrailingSlashes(path.normalize(info.position.words[1].value)) !== root);
    if (cdOutsideRoot) return false;
  }
  return true;
}

function decision(command, root, home) {
  const context = {
    root: path.normalize(root),
    home: path.normalize(home),
    protectedVariables: new Set(),
    watcherPatterns: new Set(),
    watcherPids: new Set(),
    assigned: new Map([[OPAQUE_NAMES, opaqueNamesOf(command)]]),
    nestedNames: new Set(),
  };
  const analysis = analyzeProgram(command, context);
  if (analysis.broadKill) return deny("broad-watcher-kill");
  if (analysis.error && analysis.protectedFound) return deny("unclassifiable-protected-command");
  if (!analysis.protectedFound) return { decision: "allow" };
  // A bare fm-watch falls through to the redirection check so `fm-watch > file` reports watcher-redirection.
  if (analysis.nodeInfos?.some((info) => info.protectedKind === "watch")) return deny("watcher-direct");
  if (analysis.nodeInfos?.some((info) => info.protectedKind === "bare" && !info.redirection)) return deny("watcher-direct", BARE_WATCHER_DIRECT_REASON);
  if (analysis.nestedProtected) return deny("watcher-nested");

  const separators = analysis.program.separators;
  if (separators.includes("&") || analysis.nodeInfos.some((info) => info.position.wrappers.includes("nohup")) || analysis.nodeInfos.some((info) => basename(info.position.words[0]?.value || "") === "disown")) {
    return deny("watcher-background");
  }
  if (separators.includes("|") || separators.includes("|&")) return deny("watcher-pipeline");
  if (analysis.nodeInfos.some((info) => info.redirection)) return deny("watcher-redirection");
  if (analysis.nodeInfos.some((info) => info.substitution)) return deny("watcher-nested");
  if (blessedProgram(analysis, context)) return { decision: "allow" };
  if (analysis.nodeInfos.some((info) => info.position.prefixAssignments > 0 || info.position.wrappers.some((wrapper) => wrapper !== "exec"))) {
    return deny("watcher-nested");
  }
  return deny("watcher-bundled");
}

// The bare name resolves through PATH to the watcher, so it gets its own message rather than one
// that names a different file. The reason code stays watcher-direct.
const BARE_WATCHER_DIRECT_REASON = "the bare name fm-watch resolves to the watcher and must not be run directly; arm the watcher with bin/fm-watch-arm.sh or run bin/fm-watch-checkpoint.sh instead";

function deny(code, reason = REASONS[code]) {
  return { decision: "deny", code, reason };
}

// Run the CLI only when invoked directly (node fm-arm-command-policy.mjs ...),
// never when imported by a sibling policy such as bin/fm-cd-command-policy.mjs.
function invokedDirectly() {
  const entry = process.argv[1];
  if (!entry) return false;
  const self = fileURLToPath(import.meta.url);
  try {
    return realpathSync(entry) === realpathSync(self);
  } catch {
    return entry === self;
  }
}

if (invokedDirectly()) {
  try {
    const args = parseArguments(process.argv.slice(2));
    if (!args.root || !args.home) throw new Error("--root and --home are required");
    const command = args.commandStdin ? readFileSync(0, "utf8") : args.command;
    if (!command) {
      process.stdout.write("allow\n");
    } else {
      const result = decision(command, args.root, args.home);
      if (result.decision === "allow") {
        process.stdout.write("allow\n");
      } else {
        process.stdout.write(`deny\t${result.code}\t${result.reason}\n`);
      }
    }
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
