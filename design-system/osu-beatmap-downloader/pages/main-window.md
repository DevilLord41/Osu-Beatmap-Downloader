# Main Window Override

This desktop-specific override supersedes the light landing-page recommendations in `../MASTER.md`.

## Direction

- Product: native desktop media browser and download utility.
- Style: modern dark minimalism, content-first, compact but calm.
- Motion: subtle native state transitions only, approximately 180 ms.
- Depth: flat surfaces with hairline borders; shadows are reserved for modal dialogs.
- Brand: osu! pink is the only navigation and primary-action accent.

## Tokens

| Role | Value |
|---|---|
| Canvas | `#0B0B0F` |
| Surface | `#121218` |
| Raised surface | `#181820` |
| Hover surface | `#1E1E28` |
| Border | `#2A2A35` |
| Strong border | `#393946` |
| Primary text | `#F5F5F7` |
| Secondary text | `#AEAEB9` |
| Muted text | `#7D7D8B` |
| Primary accent | `#F45D9B` |
| Accent hover | `#FF70AE` |
| Accent tint | `#391D2B` |
| Success | `#64D2A6` |
| Info | `#76A9FA` |
| Warning | `#E8B85C` |
| Danger | `#F87171` |

## Typography

- Use Segoe UI on Windows with egui's bundled proportional font as fallback.
- Page title: 22 px semibold visual weight.
- Section title: 17-18 px semibold visual weight.
- Body: 13.5 px.
- Metadata: 11-12 px with AA contrast.

## Components

- Use an 8 px spacing rhythm with 4 px for tight metadata gaps.
- Controls are 38-40 px high with 9 px radii.
- Result rows are fixed at 88 px for stable virtualization.
- Cards use a 12 px radius and a 1 px border, without persistent shadows.
- Icon-only actions use consistent hand-drawn vector strokes and descriptive accessible labels.
- Original official mode assets remain raster because they are brand-provided artwork.
- Modals use a 16 px radius, a strong backdrop, and one restrained shadow.

## Behavior Guardrails

- Preserve search, filtering, pagination, queue, login, preview, and download behavior.
- Clicking the selected mode icon again returns to all modes; do not add an All text button.
- Reserve image space before covers load to prevent layout shifts.
- Never use color as the only state indicator; pair it with text or a distinct icon.
- Avoid gradients, neon glows, glass blur, oversized typography, and decorative animation.
