---
name: setup
description: Show the captain's current setup answers when the captain invokes /setup, asks what is configured, or asks to change a setup choice such as the worker harness, backend, branch prefix, or permissions. It only reads and reports; the questions themselves run in a terminal through `mcao setup`.
user-invocable: true
metadata:
  internal: true
---

# setup

Run `bin/mcao-setup.sh --show` and report the result in plain language: which choices are answered, which are deferred to a default, and which are still unanswered.
Setup settings that nothing reads yet are labelled "saved; not yet applied" in that output, so say so rather than implying they take effect.

Then give the captain this hint: "Run 'mcao setup [section]' in a terminal to answer setup questions."
The sections are `identity`, `harness`, `backend`, `permissions` and `dispatch`.

Never prompt from here and never write a setup file by hand.
The questions need a terminal, and `bin/mcao-setup.sh` is the single owner of what each answer writes.
For a scripted change, point the captain at `mcao setup --answers <file>`.
