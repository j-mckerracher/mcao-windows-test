# Candidate panel-assignment fixtures

Status: fixtures approved as authored on 2026-10-20 in the DPES-762 policy owner and DPES-764 mapping owner roles.
That approval covers the fixture files as written and nothing else.
The per-class mapping is a derived candidate, not an approved mapping, and DPES-764 still holds it BLOCKING.
DPES-762, DPES-764, DPES-765, and DPES-769 remain Draft.
These fixtures do not satisfy AC8 or T9 of DPES-781 by themselves, and no panel was run to produce them.
Model ids in `model-capabilities.md` are placeholders, not catalog facts.

Approval belongs to the owners named by role, not to the author.

- DPES-762 policy owner: the assignment class definitions and the decision contract.
- DPES-764 mapping owner: the stage and panel-role mapping for each class.
- DPES-765 owner: the actual-diff escalation example.

The `manifest.json` file lists every fixture with its class, intent, surfaces, risk, derived mapping, sources, and approval status.
A fixture with a `panel_request` is parsed by the existing consumers: `bin/fm-capability-schema.sh validate` for `model-capabilities.md` and `bin/fm-panel-resolve.sh resolve` for each request.
Parsing proves format only, and it does not prove a panel ran or a seat completed.

## Open questions

These are listed, not resolved.

1. Eligibility for `small-low-risk`, `standard`, and `high-risk-broad` is not defined in any retrieved issue. Only `documentation-only` has a definition.
2. The stage and panel-role mapping for every class is BLOCKING in DPES-764, so the roles used by `small-low-risk` are unapproved.
3. `documentation-only` has no panel request because `fm-panel-resolve.sh` refuses a zero-seat request, so zero panel roles cannot be expressed as a file.
4. `standard` and `high-risk-broad` have no panel roles derivable from retrieved text, so no request is written for them.
5. Two retrievals place the `documentation-only` wording in different issues (DPES-761 epic and DPES-762 section 5), and the authoritative wording needs confirming before approval.
6. The high-risk obligations that the DPES-765 AC2 escalation applies are not defined.
7. The capability entries use placeholder model ids, so resolution here is not evidence about any real model.
8. The DPES-765 owner has not approved the escalation example.
