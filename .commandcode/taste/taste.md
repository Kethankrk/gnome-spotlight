# Taste

- When asking the user a question in the harness, use the question tool (`ask_user_question`) with predefined answer choices rather than asking questions as plain text. Confidence: 0.9
- The issue tracker (agent-todo) is only for specs and tickets; Git/GitHub is the source of truth for history. Push branches to the remote and open pull requests on GitHub — don't stop at committing locally on the grounds that the tracker isn't GitHub. Confidence: 0.85
- Wants README/setup docs written as concrete, step-by-step build-and-install guides that are easy to follow and to the point (commands verified by actually running them). Confidence: 0.65
- Diagnose from evidence (logs, direct reproduction) and fix the actual root cause rather than guessing or making speculative changes; avoid over-engineering and fixing things that were not asked for. Confidence: 0.85
- When adapting code to a newer platform/API version, consult the official upstream migration docs to enumerate all breaking changes and fix them comprehensively in one pass, instead of guessing and patching one error at a time. Confidence: 0.6
