// Sentinel scanner for evidence and logs (DPES-868 AC6).
//
// Evidence must carry no credentials, prompts, source, transcripts, or signed URLs.
// The scanner looks for credential and signed-URL shapes plus the fixed literal strings
// the fixture uses as prompts and tool output. A clean scan is necessary, not sufficient:
// the fixture also never records that text in the first place.

const LITERALS = [
	"probe prompt",
	"streamed chunk",
	"fixture checkout line",
	"recovered answer",
	"safe tool recovered",
	"unsafe tool interrupted",
	"could not read the checkout",
	"simulated provider denial",
];

export const SENTINEL_PATTERNS = [
	{ name: "google-oauth-token", re: /ya29\.[A-Za-z0-9_-]{10,}/ },
	{ name: "google-api-key", re: /AIza[0-9A-Za-z_-]{20,}/ },
	{ name: "private-key-block", re: /-----BEGIN [A-Z ]*PRIVATE KEY-----/ },
	{ name: "bearer-token", re: /Bearer [A-Za-z0-9._~+/-]{16,}/ },
	{ name: "signed-url-goog", re: /X-Goog-(Signature|Credential)=/i },
	{ name: "signed-url-aws", re: /X-Amz-(Signature|Credential)=/i },
	{ name: "signed-url-generic", re: /[?&]Signature=[A-Za-z0-9%_-]{8,}/ },
	// Named by index, not text, so a hit reported in a failure message never repeats the literal.
	...LITERALS.map((text, i) => ({ name: `fixture-literal-${i}`, re: new RegExp(text.replace(/ /g, "\\s+"), "i") })),
];

// Returns the names of every sentinel pattern that matches. Empty array means clean.
export function scanForSentinels(text) {
	return SENTINEL_PATTERNS.filter(({ re }) => re.test(text)).map(({ name }) => name);
}
