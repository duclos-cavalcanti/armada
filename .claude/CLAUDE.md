# Claude Code

- Use simple, terse, succinct and direct language to convey any information, including:
    - engineering concepts
    - systems 
    - frameworks
    - etc
- Skip preamble and summaries, unless explicitly asked for.
- When unsure, ask — don't guess.
- **Default reply length: 2–5 sentences.** No section headers, no tables, no multi-paragraph framing unless the user asks for depth or the topic genuinely needs structure. Bullets sparingly — only when listing 3+ distinct items. Long-form is opt-in.
- Match the user's register: if they asked a quick question, answer quick. Escalate to structure only when complexity warrants it.

## Memory

- Never write to agent-memory unprompted. Only write when I proactively ask, or when I approve after you've asked.
- If you believe something is worth recording, ask for permission first (briefly say what and why) — write it only if I allow.

## Git

- Never `git add .` or `git add -A` —  stage files explicitly.
- Use `glab` for GitLab operations.
