---
version: alpha
name: Frame
description: A focused YouTube-to-MP4 workbench inspired by the framing marks of a film viewer.
colors:
  background: "#f1f3f6"
  surface: "#ffffff"
  ink: "#1c2535"
  muted: "#626d80"
  primary: "#2754d7"
  primary-hover: "#1942b8"
  primary-active: "#12318d"
  on-primary: "#ffffff"
  border: "#dce1e9"
  danger: "#a72c35"
  danger-surface: "#fff2f1"
  success: "#23724d"
  success-surface: "#e9f5ee"
  soft: "#f7f8fb"
  scrollbar: "#9ba6b8"
  scrollbar-hover: "#717e94"
  scrollbar-active: "#4c5970"
  overlay: "#1c253566"
typography:
  display:
    fontFamily: "'Arial Narrow', 'Aptos Display', 'Helvetica Neue', Arial, sans-serif"
  body:
    fontFamily: "'Aptos', 'Segoe UI', 'Helvetica Neue', Arial, sans-serif"
    fontSize: "16px"
    lineHeight: "1.5"
  mono:
    fontFamily: "'SFMono-Regular', Consolas, 'Liberation Mono', monospace"
rounded:
  control: "8px"
  panel: "16px"
spacing:
  unit: "8px"
  panel: "32px"
  page-max: "1040px"
components:
  button:
    height: "48px"
  field:
    height: "58px"
  dialog:
    width: "690px"
---

# Frame design system

## Overview

Frame is a public, single-task product: paste a public YouTube link, confirm the video in the official YouTube player, generate an MP4, then play or download it. The current user request is the product brief; [the API contract](docs/api.md), `src/web.rs`, and `src/extractor.rs` constrain accepted links, anonymous browser ownership, and asynchronous generation. There was no earlier frontend or sibling screen to preserve; the converter and confirmation dialog establish the shared language.

The visual reference is a quiet desktop film viewer: cool silver casing, ink typography, cobalt controls, and one four-corner framing mark around the empty player. The mark repeats subtly in the wordmark and MP4 headline. This is a product register, not a marketing landing page. Large typography gives the tool an identity while the form and media controls stay familiar. Avoid competing cards, decorative gradients, invented metrics, and dashboard navigation for a one-field task.

The audience is English-speaking visitors on desktop and mobile; no Japan-specific market or locale is specified. Owned UI uses English and locale `en`; no currency, date entry, or time-zone display exists. External YouTube copy belongs to its embedded player. Never infer permission to save a video from a link alone.

**Token ownership:** This file defines the normative palette, type families, spacing, and radii. Runtime implementation is the single `:root` block in [web/styles.css](web/styles.css). Colors map mechanically from `colors.<name>` to `--color-<name>`, typography from `typography.<role>.fontFamily` to `--font-<role>`, radii from `rounded.<name>` to `--radius-<name>`, and spacing to `--space-unit`, `--space-panel`, and `--page-max`. No generated adapters or second theme package exist. Change normative values and that block together; inspect browser-computed values during review. At ≤560px, the converter intentionally overrides panel padding to 20px and radius to 12px for usable phone geometry.

## Colors

Silver `background` surrounds a white `surface`; `soft` separates the empty/progress media area. `ink` is the default text; `muted` carries instructions and metadata. Cobalt `primary` means action and keyboard focus. Green `success` accompanies both a check and completion text. Red `danger` accompanies explicit errors; color is never the only signal. Borders provide quiet structural separation, not the only affordance for an interactive field.

Only light mode is supported. The `forced-colors` media query delegates scrollbars, outlines, and controls to system colors. The scrollbar track uses background; thumb, hover, and active use the three scrollbar tokens globally for all product-owned overflow regions. Cross-origin player scrollbars are outside this contract.

## Typography

The compact display stack carries the wordmark and headings; the complementary system body stack carries controls and prose. Local font stacks avoid a download, font swap, or third-party request. Monospace is reserved for file format, video ID, phase, and short process captions. Display size is 58–86px on large screens, with mobile reflow; body is 16px/1.5 and controls 14–16px. Headings use restrained tight tracking, never body text. Labels remain sentence case; small section captions may use uppercase. Long error text and technical values wrap rather than truncate.

## Layout

The page has natural document scrolling and a 1040px maximum work area, with a single converter below the heading. At 840px, the supporting introduction moves under the headline. At 560px, the form stacks vertically, actions span usable widths, and the headline uses two lines. The 8px base rhythm drives 16/24/32px group spacing. The input and primary button keep their dimensions when busy. The validation and connection-status slots reserve space; players reserve 16:9 before media arrives.

The native modal is bounded by the dynamic viewport with 16px edge clearance and internal scrolling; it does not impose a fixed height on the document. The toast respects the bottom safe area. The global stable scrollbar gutter prevents opening the dialog from shifting the converter. Progress panels grow naturally when status changes; no full-screen loading overlay obscures the form.

## Elevation & Depth

Static surfaces use borders and tonal separation without shadows. The modal and toast alone use `--shadow-dialog: 0 24px 80px #1c25352b`. The native dialog owns the browser top layer and its translucent ink backdrop. Toasts move inside the modal while open so they remain visible and accessible above its inert background; their global layer is `--z-toast: 900`. The skip link uses `--z-skip: 100`.

## Shapes

Controls and media have 8px corners, containers 16px, and small format/phase labels 4px. The signature film frame uses open square corners and a simple play triangle. Circular close and success marks are utility exceptions. Icons use a consistent thin rounded stroke, 18–20px in actions; meaningful actions always retain a text label.

## Components

Buttons have solid brand and outline neutral variants. Enabled controls define hover, focus-visible, and pressed behavior; disabled controls dim, stop interaction, and show a non-interactive cursor. Busy generation disables the form without changing its label or width and exposes `aria-busy`. Native `<progress>` is determinate only when the API provides a finite percentage; otherwise its indeterminate state truthfully communicates unknown progress. No decorative spinner or skeleton is used.

The labeled URL form owns inline validation and preserves input on failure. The confirmation uses native `<dialog>` with an accessible name, description, initial Cancel focus, modal isolation, Tab cycling, Escape, and focus restoration. The official privacy-enhanced YouTube iframe is created only after the first action and removed when dismissed. Only Continue starts generation. An embed failure includes an Open on YouTube fallback alongside an error toast.

One toast system acknowledges success, information, and errors. Errors persist until dismissed or replaced and also have inline text; success/info expire after six seconds, paused on pointer hover or keyboard focus. Toast copy uses safe DOM text. Download stays an explicit, labeled action, separate from video playback. The browser's native video controls own playback, audio, and fullscreen behavior.

Only 100–150ms control feedback transitions are used. Reduced-motion disables all optional transitions and animations. Product voice uses concrete actions: Generate MP4, Continue to generate, Cancel generation, Download MP4. File size uses `Intl.NumberFormat('en')`. Do not invent a title, percentage, or estimated completion time absent from the backend.

## Do's and Don'ts

- Do keep the link, confirmation, status, result, and recovery in one focused workflow.
- Do reuse the existing token block, buttons, notice, toast, and dialog for future screens.
- Do preserve server error messages as text with a useful next step.
- Don't expose the private API token or put credentials in browser storage.
- Don't auto-generate on paste, preview, modal dismissal, or an uncertain create retry.
- Don't claim live extraction compatibility from a controlled browser fixture.
