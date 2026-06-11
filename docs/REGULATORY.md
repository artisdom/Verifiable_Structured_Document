# Regulatory Wedge Dossiers

ROADMAP §9: *the real moat is political, not technical* — PDF/A is
written into statute in many jurisdictions. VSD's adoption wedge is
regulation that PDF satisfies poorly. These dossiers map concrete
regulatory requirements to format properties, with the implementation
evidence for each claim. Audience: procurement evaluators, policy
teams, and standards advocates.

> Disclaimer: engineering analysis, not legal advice. Citations name
> the instruments; applicability to a given deployment needs counsel.

---

## 1. Accessibility — European Accessibility Act (EAA)

**The requirement.** Directive (EU) 2019/882, applicable since June
2025, requires e-books, e-commerce, banking and other consumer-facing
digital services — including the documents they deliver — to be
accessible, in practice per EN 301 549 / WCAG. PDF/UA (ISO 14289)
exists precisely because ordinary PDF fails this; remediation of
existing PDFs is an entire industry.

**Why PDF struggles.** Accessibility in PDF is a parallel,
optional "tagged" structure bolted onto draw commands. Most producers
never write it; nothing in the format makes an untagged PDF invalid;
remediation is after-the-fact guesswork.

**The VSD property.** Accessibility is a **validity condition**, not a
feature tier:

- the canonical layer *is* the semantic structure — reading order is
  tree order, tables carry real topology and header scope (spec §5);
- a figure without alt text (and not marked decorative) is a
  **malformed document** — `E_ALT_TEXT`, enforced at validation and at
  authoring (the Markdown importer refuses such images);
- exported PDFs are tagged automatically from the same structure
  (`vsd export` emits H1–H6/P/Caption + `/Alt`), typically better than
  native PDFs.

**Evidence.** `vsd-core/src/validate.rs` (`E_ALT_TEXT`);
`alt_text_is_a_validity_condition` test; tagged export assertions in
`vsd-pdf/tests/pdf.rs`.

## 2. Machine readability — e-invoicing (EN 16931 world)

**The requirement.** EU Directive 2014/55/EU and the spreading B2B
mandates (Italy FatturaPA, France, Germany's Wachstumschancengesetz
timeline, Poland KSeF…) require *structured*, machine-processable
invoices. The dominant compromise — ZUGFeRD/Factur-X — staples XML
inside a PDF because neither format alone serves both humans and
machines.

**Why PDF struggles.** Extracting a table from PDF is a research
field. The hybrid XML-in-PDF approach concedes the point: the visual
document and the data are separate artifacts that can silently
disagree — there is no mechanism binding the pixels to the XML.

**The VSD property.** One artifact, both audiences, provably
consistent:

- tables, fields, and values are structural facts — extraction is a
  tree walk (spec §5), and the filled-form layer carries typed values
  with evaluated constraints (spec §9);
- the render cache is a *verifiable projection* of that same data:
  `vsd verify --recompute` proves the human-visible pages match the
  machine-readable content — the exact guarantee the ZUGFeRD
  architecture cannot offer;
- for transport into PDF-mandated channels, the hybrid export embeds
  the canonical VSD inside the PDF, recoverable and verifiable by id.

**Evidence.** `extract.rs` exactness tests; `verify --recompute` and
the grafted-cache detection test; `hybrid_round_trip_is_the_identity_function`.

## 3. AI provenance — EU AI Act and procurement checklists

**The requirement.** Regulation (EU) 2024/1689 Art. 50 requires
machine-readable marking/disclosure of AI-generated content;
public-sector procurement increasingly asks "how do we know what an AI
touched?". C2PA is the emerging industry vehicle.

**Why PDF struggles.** Metadata (XMP) is unbound to content — it
survives neither edits honestly nor strips loudly. Nothing ties "an AI
made this" to *which bytes* the AI made.

**The VSD property.** Provenance assertions anchor **manifest hashes**
(spec §12): `ai-generated {model, params-hash}` commits to the exact
revision it describes, the chain is append-only via predecessor links,
and stripping it changes the document id — visible to anyone holding
the old id, a signature, or a transparency-log entry (spec §14).
`vsd provenance add --kind ai-generated …` is one command.

**Evidence.** provenance commands + spec §12; tlog inclusion/
consistency tests; C2PA claim-serialization interop tracked open in
ROADMAP 5c.

## 4. Redaction — court filings and FOIA

**The requirement.** Court rules (e.g. FRCP 5.2 redaction practice)
and FOIA/OIA processing require removed content to be *gone*. The
failure mode is famous: black rectangles over live text, recoverable
by select-and-copy — recurring across courts, regulators, and
militaries for two decades.

**Why PDF struggles.** Redaction is a drawing operation unless
specialized tooling is used correctly; the format happily represents
"covered but present", and stale caches/metadata leak even when the
text layer is handled.

**The VSD property.** Failed redaction is **unrepresentable** (spec
§11.1): the operation replaces the subtree, purges unreferenced
objects, and drops every derived layer that could quote the content
(render cache, page index, filled values). A `proof` hash lets a court
later verify what was removed against an escrowed original, without
the filing containing it. There is no overlay construct to misuse.

**Evidence.** `redaction_never_leaks` property test (byte-scans every
container and object on generated documents); `redaction_destroys_content`;
conformance vector `valid/redacted.vsd` with `must_not_contain`.

## 5. Long-term archival — PDF/A's actual job

**The requirement.** National archives and records acts mandate
formats that remain readable and verifiable for decades (OAIS-style
preservation; PDF/A-anchored regulations).

**The VSD property.** VSD/Archive requires embedded fonts, explicit
provenance, and a render cache pinned to a **versioned, normative
layout engine** — re-renderable bit-identically by any future
implementation of the contract (docs/LAYOUT-1.0.md), with integer-only
arithmetic so platform drift is impossible. Hybrid Ed25519+ML-DSA-65
signatures (spec §10.2) address the quantum horizon archives actually
plan for. Identity survives container recompression, so storage-layer
migrations never invalidate signatures.

**Evidence.** cross-OS golden layout hash in CI; hybrid signature
tests; `identity_survives_recompression`.

---

## Engagement order (suggested)

1. **Accessibility** — the EAA is in force now, remediation budgets are
   real, and "validity condition" is a one-sentence differentiator.
2. **E-invoicing** — mandates are spreading country by country with
   open consultation windows.
3. **AI provenance** — obligations phase in through 2026–2027;
   procurement language is being written today.
4. **Redaction/archival** — court-tech and records-management
   modernization cycles; longer sales cycle, strongest demo
   (`vsd redact` + the leak-impossibility tests).
