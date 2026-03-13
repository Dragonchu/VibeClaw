---
description: Enforces a "Code as Documentation" philosophy by prohibiting redundant docstrings, post-coding summaries, and high-maintenance comments.
paths:
  - '**/*'
---

# Rule: Minimalist Documentation & Clean Code

## Core Principle

Prioritize expressive, self-documenting code over external comments or summaries. Code should be readable enough that it explains "what" and "how" through naming and structure.

## Mandatory Constraints

1. **No Post-Coding Summaries:** After providing code, do not write a paragraph summarizing what the code does unless explicitly asked. The code must speak for itself.
2. **No Redundant Comments:** Do not write comments that state the obvious (e.g., `i++; // increment i`).
3. **No External Doc References:** Strictly avoid embedding links or references to external documentation/URLs within code comments, as these inevitably become stale.
4. **No "Tombstones" or Headers:** Do not add file headers with timestamps, author names, or changelogs.
5. **No JSDoc/Docstrings by Default:** Only add formal documentation (JSDoc, Pydoc, etc.) for complex public APIs or exported library functions where the logic is non-trivial.

## Behavioral Expectations

- **Self-Documenting Naming:** Spend effort on descriptive variable, function, and class names instead of explaining them in comments.
- **Explain "Why", Not "What":** Only use comments to explain non-obvious business logic or "why" a specific workaround was necessary.
- **Delete, Don't Comment:** If code is no longer needed, delete it. Do not leave "commented-out" code blocks.

## Communication Style

When the task is done, simply provide the code. If the user asks "How does this work?", then and only then provide a brief explanation.
