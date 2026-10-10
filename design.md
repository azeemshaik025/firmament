# Design — Firmament

A locked design system for the Firmament web app and landing. Every page reads
this file before emitting styles. Extend or amend this file when the system
needs to grow; do not improvise per page.

<!-- Hallmark · genre: modern-minimal · theme: custom (firm point) · studied: yes · DNA-source: url (public reference, structure only) -->

## Essence

Firmament is the vault of fixed stars, and the product promise is a number that
does not move. The identity is one mark: a horizon rule with a single fixed
point on it. The mark is the favicon, the dot in the wordmark, and, on the Pay
page, the rate lock itself: the line drains for the quote window while the
point stays put.

Tone: austere-technical with warmth. Calm, exact, settled. Never dark-mode
atmospheric, never comic, never a dashboard full of colour.

## Genre

modern-minimal. Light warm paper, cool ink, one accent, pill controls, no
drop shadows, no gradients, no glass.

## Macrostructure family

- Marketing (landing): Split Studio. Statement left, a real product figure
  right; sections alternate text and proof. Nav N9 edge-aligned (wordmark left,
  GitHub right). Footer Ft5 statement.
- App (Pay, Swap, Maker): Workbench. Function carries the page; no enrichment.
  Compact top bar with three destinations; two-column desk on Pay and Swap;
  calm tables on Maker.
- Content (docs): typography only.

## Theme

- `--color-paper`        oklch(97% 0.007 80)
- `--color-paper-2`      oklch(94.5% 0.008 80)
- `--color-paper-3`      oklch(91% 0.010 80)
- `--color-rule`         oklch(86% 0.009 80)
- `--color-rule-strong`  oklch(76% 0.012 80)
- `--color-ink`          oklch(21% 0.018 265)
- `--color-ink-2`        oklch(34% 0.016 265)
- `--color-muted`        oklch(48% 0.012 265)
- `--color-faint`        oklch(62% 0.010 265)
- `--color-accent`       oklch(74% 0.150 78)   the star; marks, rings, the lock point
- `--color-accent-ink`   oklch(42% 0.110 70)   accent as text, when text must be accent
- `--color-focus`        oklch(60% 0.150 78)
- `--color-good`         oklch(52% 0.110 155)  delivered
- `--color-bad`          oklch(50% 0.160 25)   failed / refunded

Accent footprint under 3% of any view. Green and red are semantic states only,
never decoration. No colour at zero chroma.

## Typography

- Display: Cabinet Grotesk 500, tracking −0.025em, line-height 1.05. Wordmark
  Cabinet Grotesk 700.
- Body: IBM Plex Sans 400 and 500, 15px base, line-height 1.55.
- Mono (outlier, one role): JetBrains Mono for machine values only — amounts,
  addresses, signatures, ledger cells. Always `tabular-nums`.
- Labels: body face, 13px, sentence case, muted. No uppercase mono eyebrows.
- Scale: 12 / 13 / 15 / 18 / 22 / 28 / 36 and a display clamp(2.25rem, 3.6vw + 1rem, 3.5rem).

## Spacing

4-point scale, named tokens in `web/app/src/tokens.css`. Pages use
`var(--space-*)`, never raw values. Page column 1080px.

## Surfaces

12px radius on panels, 8px on inputs, pill on buttons. One hairline border,
no shadows. Nested panels are not allowed: one containment layer per block.

## Motion

- Easings `--ease-out` cubic-bezier(0.16, 1, 0.3, 1), `--ease-in`
  cubic-bezier(0.7, 0, 0.84, 0), `--ease-in-out` cubic-bezier(0.65, 0, 0.35, 1).
- Three things move in the app: the lock line (linear, functional), step
  completion (opacity, 220ms), route change (150ms opacity).
- Reveals: none. Reduced motion collapses everything to ≤150ms opacity.

## Microinteractions

- Silent success: the receipt is the confirmation; no toasts for success.
- Button press: background shift 220ms, `translateY(1px)` on active.
- Copy: label swaps to "Copied" for 1.4s, no toast.
- Focus rings instant, 2px accent, 3px offset.

## CTA voice

- Primary: ink-filled pill, paper text, 14px/500, 44px tall. Copy is a verb
  with the amount when known: "Pay 1.12 USDC".
- Secondary: hairline pill, ink text, same height.
- Tertiary: underlined text link, no box.

## What pages MUST share

- The horizon mark and the wordmark.
- The accent and its placement.
- The three faces and their roles.
- Button shape, radius, hairline.
- Sentence-case labels; no section numbering.

## What pages MAY differ on

- Macrostructure within the family (the landing alternates split blocks; the
  app pages are desks and tables).
- The landing may carry one figure: a real Pay quote rendered as itself.
