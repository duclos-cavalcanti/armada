# demos — TODO

## Adopt from `architecture-diagrams` (Angel's repo)

`gitlab.com/ripple/ai/custody/architecture-diagrams` is a mature, CI-published
Managed-Custody diagram system (Svelte 5 + Svelte Flow, auto-published to
`d391w6567k7kxu.cloudfront.net/managed-custody` on merge). Our `demos/architecture`
viewer is a parallel React reimplementation; these are the ideas worth pulling in.

1. **Formal JSON Schema + validating CLI.**
   They ship `schemas/diagram.schema.json` and `bun diagram validate|complete <file>`
   — `validate` fails on bad ids / wrong enums; `complete` reports which fields a
   diagram still leaves empty ("hand over with no required gap"). We hand-author
   JSON with zero validation. Adopt: a schema for our graph JSON + a check step.

2. **`flows` / story mode (animated walkthroughs).**
   A `flows[]` plays one request as a movie — scene by scene, lighting the cards +
   arrows involved, showing the real request/response bodies (exchanges, beats,
   session pills, payloads). This is the original diagram-studio interactivity we
   were emulating; far richer than our static `detail.modes` tabs + before/after
   toggles. Adopt: a flow/step overlay model.

3. **C4 two-level discipline.**
   Every diagram is two levels — a left-to-right container overview that drills
   into the full component picture — with deeper detail in drawers, not on the
   canvas. The principled answer to the density problem we fought with the global
   `SPREAD` factor. Adopt: containers-then-components, detail in panes.

4. **CLI single-file HTML export.**
   `bun diagram html <file>` emits one self-contained HTML with the viewer. We
   reinvented this with `vite-plugin-singlefile`. Adopt their shape / fold ours in.

5. **CI auto-publish + MR previews.**
   `.gitlab-ci.yml` publishes the site to CloudFront on every merge to `main`, and
   each MR gets a preview URL (auto-stopped 2 days after last deploy). This is the
   real "automation" — merge → live. Adopt: a publish-on-merge pipeline.

6. **Grounding sync scripts.**
   `sync-mady-spec`, `sync-jira-history`, `update-architecture-statuses` keep the
   diagrams pinned to code + tickets automatically, instead of our manual
   per-diagram grounding. Adopt: a scripted re-ground / status-refresh.

## In progress

- **`fleet-publish` skill** — build `demos/architecture` to a single HTML and
  publish it to a Splash page (SSO handled via the vendored Chrome extension's
  session hand-off). Lives at the fleet (repo) root: `.claude/skills/fleet-publish/`;
  points at `demos/architecture/` for the build + `splash-page.json` state.
