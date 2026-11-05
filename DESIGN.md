---
version: alpha
name: Frame
description: A precise, quiet editing workspace for authenticated YouTube extraction.
colors:
  primary: "#5558d9"
  background: "#f7f8fa"
  surface: "#ffffff"
  text: "#191b22"
  muted: "#626570"
  dark-primary: "#a5a7ff"
  dark-background: "#111216"
  dark-surface: "#191a20"
  dark-text: "#f1f2f5"
  dark-muted: "#a3a6b2"
typography:
  display:
    fontFamily: '"Avenir Next", Avenir, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif'
  body:
    fontFamily: '"Avenir Next", Avenir, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif'
  mono:
    fontFamily: '"SFMono-Regular", Consolas, "Liberation Mono", monospace'
rounded:
  sm: "8px"
  md: "12px"
  lg: "16px"
spacing:
  unit: "4px"
  page-max: "1120px"
components:
  button:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.surface}"
    typography: "{typography.body}"
    rounded: "{rounded.sm}"
  field:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    rounded: "{rounded.sm}"
  canvas:
    backgroundColor: "{colors.background}"
    textColor: "{colors.muted}"
  dark-button:
    backgroundColor: "{colors.dark-primary}"
    textColor: "{colors.dark-background}"
  dark-field:
    backgroundColor: "{colors.dark-surface}"
    textColor: "{colors.dark-text}"
  dark-canvas:
    backgroundColor: "{colors.dark-background}"
    textColor: "{colors.dark-muted}"
---

# Frame design system

## Overview

Frame is a single-operator extraction tool, not a media catalog. An authorized operator identifies a YouTube source, confirms it, chooses an output, and follows a bounded server job to a locally downloadable file. Product facts come from [API v1](docs/api.md), [the route implementation](src/api.rs), [request validation](src/model.rs), and [the contributor invariants](AGENTS.md).

The north star is a quiet video edit bay: one source, explicit output settings, and a dependable render surface. The visual signature is the four-corner frame used in the mark and empty media surface. The ordered source/configuration/export trail represents real workflow state. The user requested a complete rebrand toward the precision and spacing of premium productivity software; this replaces the former blue/coral identity.

The interface is English and globally usable. No country-specific business behavior is inferred. Desktop operators and narrow mobile screens receive the same workflow. No research interviews or usability sessions are claimed; operator needs are inferred from the current API and task.

## Colors

Neutral zinc surfaces carry hierarchy. Periwinkle is reserved for primary actions, selected states, and keyboard focus. Green means completed work; red means an actionable failure. Labels accompany all semantic colors. Light and dark themes change surfaces and contrast, not placement or meaning. Forced colors defer to system colors.

Canonical runtime values live in `web/app.css`; this file mirrors their accepted meaning (ownership model B). CSS custom properties feed every shared control and state. No independent theme package or generated adapter exists. When tokens change, reconcile these frontmatter values and inspect rendered contrast in both themes in the same branch.

| Document color | Runtime property | Consumers |
| --- | --- | --- |
| primary / dark-primary | --primary | Primary buttons, links, focus, current step |
| background / dark-background | --canvas | Page and scrollbar track |
| surface / dark-surface | --surface | Panels, inputs, dialog, toasts |
| text / dark-text | --ink | Headings and form labels |
| muted / dark-muted | --ink-quiet | Supporting copy and control utilities |

`--ink-faint` uses #6d707c in light mode and #9699a6 in dark mode for compact labels; both pass 4.5:1 against their canvas. Radius frontmatter maps directly to `--radius-sm/md/lg`; typography maps to `--font-sans` and `--font-mono`; page-max maps to the shared 1120px shell/header width. No framework adapter is involved.

## Typography

Use the locally available Avenir Next/system stack, with display sizing reserved for the page heading and brand. The same family keeps forms familiar and avoids network font shifts. Monospace is reserved for workflow indices, job identifiers, and compact file details. Body copy uses sentence case, approximately 1.5 line height, and short measures. Inputs remain legible on mobile. All families have system fallbacks; no remote font requests are required.

## Layout

Use a compact header and a centered workspace at a maximum of 1120px. The URL field is the first prominent interaction. On wide screens, source and configuration sit beside the output surface; narrow screens stack them with natural document scrolling. The input and output are workflow peers, not dashboard widgets. Keep output space available across empty, processing, success, and error states.

The spacing rhythm is 4px with larger 16/24/32px groupings. Reserve media aspect ratios and button dimensions during busy states. Dialog content scrolls within the viewport, with close and back actions reachable. Long filenames, URLs, and UUIDs wrap without horizontal document overflow.

## Elevation & Depth

Use quiet one-pixel borders and small shadows where a surface separates from the canvas. Modal elevation is stronger than card elevation. Dark mode uses tonal separation. Do not introduce decorative gradients, backdrop blur, glass surfaces, floating ornaments, or persistent animation.

## Shapes

Controls use restrained 8px corners, grouped surfaces 12px, and major panels 16px. Frame corners provide the single memorable geometry. Small status badges may be pills. Icons use a consistent outline stroke; keep text on actions whose icon is ambiguous.

## Components

`web/app.css` owns shared button, field, panel, status, scrollbar, toast, and dialog styles. `web/app.js` owns their behavior. [UX-CONTRACT.md](UX-CONTRACT.md) maps the canonical owners and lifecycle.

Buttons have hover, pressed, focus-visible, disabled, and busy states. Solid primary actions advance the task; outlined and quiet actions go back or recover. Cancellation is distinct from the primary action. Success follows server-confirmed completion and authenticated file transfer.

Native HTML selects are intentional: the platform owns popup geometry and keyboard interaction. The product owns trigger styling, labels, values, and validation. Forms use app-owned inline validation with `novalidate`, field associations, and first-error focus. Tokens are masked by default with an accessible show/hide control. Confirmation uses native `<dialog>` focus isolation and Escape behavior.

Progress is numeric only when the server supplies a finite percentage. Otherwise use an indeterminate activity treatment and a specific stage label. Do not invent intermediate progress. All output, error, and processing views share a stable surface and truthful status text.

Transitions are short (roughly 150–180ms), restricted to interaction feedback. Respect reduced motion. Scrollbars remain visible and inherit global thumb, track, hover, active, and forced-color rules.

## Do's and Don'ts

- Do keep one clear next action and preserve entered configuration after recoverable failure.
- Do make job identity and retry/recheck actions available when a response is uncertain.
- Do keep the token in tab memory and fetch files with same-origin bearer authentication.
- Don't treat queue admission as successful extraction, promise availability, or fake media metadata.
- Don't add frameworks, decorative imagery, external assets, or redundant dashboard furniture.
- Don't let theme changes alter workflow or reduce keyboard access.
