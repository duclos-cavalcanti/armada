# open-slide — Agent Guide (fleet)

You are authoring **slides** for the fleet, the factory itself (fleetd, fleetc, the cogs). Every slide is arbitrary React code that you write.

## Hard rules

- This folder is the fleet's open-slide slides folder: wherever a skill says `slides/`, it means this folder.
- Put your slide under `./<kebab-case-id>/`. The entry is `./<id>/index.tsx`.
- Put slide-specific images/videos/fonts under `./<id>/assets/`. There is no shared `assets/` folder: a deck's assets stay with the deck.
- Do **not** touch the open-slide setup at `demos/` (`package.json`, `open-slide.config.ts`), or other decks.
- Do not add dependencies. Use only `react` and standard web APIs.
- Serve these decks with `make slides DEMO=fleet` (http://localhost:5176).

## Which skill to use

The skills live at `demos/.claude/skills/`, installed by `npm install` in `demos/`.

- **Drafting a new deck** — use the `create-slide` skill. It walks through scoping questions, structure, and hand-off.
- **Applying inspector comments** (`@slide-comment` markers in a page) — use the `apply-comments` skill.
- **Creating or extracting a theme** — use the `create-theme` skill. Themes live as markdown under `demos/themes/<id>.md` (the folder is created with the first theme) and are read by `create-slide` before authoring.
- **Resolving "this page" / "this element"** — when the user references the current slide or selection without naming it, consult the `current-slide` skill. It reads the dev server's `demos/node_modules/.open-slide/current.json` to find which slide, page, and inspector-picked element they mean.
- **Writing a speech script / speaker notes** — use the framework's built-in feature: the `notes` export in the slide's `index.tsx`, index-aligned with the page array and shown in the presenter view. See the **Speaker notes** section of the `slide-authoring` skill. Never deliver a script as a markdown or text file.
- **Any other slide edit** — read the `slide-authoring` skill before writing. It is the technical reference for everything inside a deck: file contract, the 1920×1080 canvas, type scale, palette, layout, assets, self-review checklist, and anti-patterns. `create-slide` and `apply-comments` both defer to it for the *how*.

Keep this file short: hard rules only. All deeper guidance lives in the skills above.

## Updating skills

The skills above are managed by `@open-slide/core`. Do not edit them in place. To pull the latest versions:

```
npm --prefix demos up @open-slide/core
npm --prefix demos run sync:skills
```

`make slides` skips open-slide's drift check; `cd demos && npx open-slide sync:skills --dry-run` previews changes without writing.
