---
name: LAN-Chat
description: A calm LAN-only messenger whose living piece is the device list
colors:
  ink: "#17181A"
  ink-soft: "#585C63"
  ink-faint: "#666A71"
  surface: "#FFFFFF"
  surface-sunken: "#F7F7F8"
  line: "#ECECEE"
  hover-wash: "#F1F1F3"
  selected-wash: "#E9E9EC"
  lavender-message: "#EFE9FB"
  lavender-ink: "#241A3D"
  cream-message: "#FBF2D9"
  cream-ink: "#383013"
  signal-green: "#2E7D46"
  offline-gray: "#A6AAB1"
  danger: "#B3261E"
  danger-wash: "#FBEAEA"
  amber-ink: "#8A5A00"
  amber-wash: "#FFF3D6"
typography:
  display:
    fontFamily: "\"Inter Variable\", -apple-system, \"SF Pro Text\", system-ui, sans-serif"
    fontSize: "22px"
    fontWeight: 650
    lineHeight: 1.25
    letterSpacing: "-0.02em"
  title:
    fontFamily: "\"Inter Variable\", -apple-system, \"SF Pro Text\", system-ui, sans-serif"
    fontSize: "15px"
    fontWeight: 650
    lineHeight: 1.4
    letterSpacing: "-0.01em"
  body:
    fontFamily: "\"Inter Variable\", -apple-system, \"SF Pro Text\", system-ui, sans-serif"
    fontSize: "14.5px"
    fontWeight: 400
    lineHeight: 1.42
    letterSpacing: "normal"
  label:
    fontFamily: "\"Inter Variable\", -apple-system, \"SF Pro Text\", system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 550
    lineHeight: 1.4
    letterSpacing: "normal"
rounded:
  pill: "999px"
  bubble: "18px"
  bubble-tail: "6px"
  field: "14px"
  row: "12px"
  tile: "9px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "20px"
components:
  button-primary:
    backgroundColor: "{colors.ink}"
    textColor: "#FFFFFF"
    rounded: "{rounded.pill}"
    padding: "10px 18px"
    typography: "{typography.body}"
  button-primary-disabled:
    backgroundColor: "#E5E5E8"
    textColor: "#9A9DA3"
    rounded: "{rounded.pill}"
    padding: "10px 18px"
  chip-status:
    backgroundColor: "{colors.surface-sunken}"
    textColor: "{colors.ink-soft}"
    rounded: "{rounded.pill}"
    padding: "5px 11px"
  chip-day:
    backgroundColor: "{colors.hover-wash}"
    textColor: "{colors.ink-soft}"
    rounded: "{rounded.pill}"
    padding: "4px 12px"
  bubble-incoming:
    backgroundColor: "{colors.lavender-message}"
    textColor: "{colors.lavender-ink}"
    rounded: "{rounded.bubble}"
    padding: "9px 12px 7px"
  bubble-outgoing:
    backgroundColor: "{colors.cream-message}"
    textColor: "{colors.cream-ink}"
    rounded: "{rounded.bubble}"
    padding: "9px 12px 7px"
  input-message:
    backgroundColor: "{colors.surface-sunken}"
    textColor: "{colors.ink}"
    rounded: "{rounded.field}"
    padding: "10px 12px"
---

# Design System: LAN-Chat

## Overview

**Creative North Star: "El Mensajero Tranquilo"**

A desktop messenger where nothing moves except the truth: devices joining and leaving the local network. Everything else — reading, writing, deleting — is still and unhurried. Surfaces are flat white over one sunken gray; depth appears only where content floats above the page (menus). The personality is the mass-market chat apps the user pinned as the craft bar (WhatsApp/Telegram Desktop), executed at production finish: pastel bubbles, generous radii, one black pill for the primary action, circular pastel avatars with initials.

**Key Characteristics:**
- Two fixed bubble pastels carry all expression: lavender for incoming, cream for outgoing
- Liveness lives exclusively in the device list (entrance animation, live count, status)
- Zero remote resources: fonts and icons are bundled; the LAN-only promise is visual too
- Honesty as ornament: disabled controls state their reason; no invented affordances

## Colors

One neutral ink family on white/sunken-gray grounds, two pastel message tints, and three small semantic voices (green, amber, red).

### Primary
- **Soft Ink** (#17181A): all primary text, the app tile, and the single black pill action. Never used as a background larger than a pill or tile.

### Secondary
- **Lavender Message** (#EFE9FB) with **Lavender Ink** (#241A3D): incoming message bubbles only.
- **Cream Message** (#FBF2D9) with **Cream Ink** (#383013): outgoing message bubbles only.

### Tertiary
- **Signal Green** (#2E7D46): "En línea" status text and online dots. Paired with the word it means.
- **Danger** (#B3261E) with **Danger Wash** (#FBEAEA): failed sends and destructive confirmations.
- **Amber Ink** (#8A5A00) with **Amber Wash** (#FFF3D6): the "Demostración" badge on synthetic data.

### Neutral
- **Sunken Ground** (#F7F7F8): list column and composer field background; the page's tonal depth layer.
- **Surface** (#FFFFFF): conversation column, top bar, menus.
- **Line** (#ECECEE): all 1px hairlines (panel borders, field strokes).
- **Hover Wash** (#F1F1F3) / **Selected Wash** (#E9E9EC): interactive row states.
- **Soft Ink Secondary** (#585C63) and **Faint** (#666A71): secondary text and metadata; both hold ≥4.5:1 on white and on both bubble pastels.

### Named Rules
**The Two Pastels Rule.** Exactly two bubble colors exist. A third message tint needs a product decision, not a design whim.
**The Color-Free State Rule.** Connection and delivery states must read without hue: dots sit next to words, offline rows dim, failed bubbles outline.

## Typography

**Display Font:** Inter Variable (bundled, with -apple-system / system-ui fallback)
**Body Font:** Inter Variable (same stack — one face, ranked by weight and size)

**Character:** A single workhorse grotesque; the interface has no display voice, only clear ranks. Numerals in times and counts are tabular.

### Hierarchy
- **Display** (650, 22px, 1.25, -0.02em): welcome heading only.
- **Title** (650, 14.5–15px, -0.01em): panel and conversation headers.
- **Body** (400, 14.5px, 1.42): bubbles, composer, device names at 590 weight.
- **Label** (500–550, 11–12.5px): status, counts, chips, timestamps (11px, tabular).

### Named Rules
**The One Face Rule.** One family, no display voice. Rank by weight (400–650) and size (11–22px), never by adding faces.

## Layout

Two panels under a 48px top bar: a 320px sunken device list and the conversation field. Below 700px the shell collapses to one panel at a time (list ↔ conversation with a back chevron). Spacing rhythm is 4-based; list rows breathe at 9–10px vertical padding, conversation content gets 18–20px. The thread anchors to the bottom (`margin-top: auto`), like every mass-market chat app.

## Elevation & Depth

Flat by default; depth is tonal (sunken gray behind white). One structural shadow exists, for floating menus only: `box-shadow: 0 12px 32px rgba(23,24,26,0.14), 0 2px 8px rgba(23,24,26,0.08)`. Nothing else on the page lifts.

### Named Rules
**The Floating-Menu Shadow Rule.** If it doesn't overlap the page, it doesn't cast a shadow.

## Shapes

Generous and circular: pills (999px) for actions and chips, 18px bubbles with a single 6px tail corner toward the author (flattened to 18px inside grouped runs), 14px composer field, 12px rows and menu items, 9px app tile. Status dots are 11px circles ringed by 2px of surface color.

## Components

### Buttons
- **Shape:** full pill (999px)
- **Primary:** Soft Ink background, white text, 10×18px padding, 600 weight
- **Hover / Active:** #2E2F33 background; active scales to 0.98
- **Disabled:** #E5E5E8 with #9A9DA3 text; never ambiguous — attach buttons carry a title stating the reason ("Adjuntar archivos: pronto")

### Icon Buttons
- 32px circular ghost buttons, #585C63 icon; hover gains Hover Wash and full ink
- The narrow-layout back button is the only one revealed conditionally

### Device Row
- 12px-radius full-width row: 40px pastel avatar with initials + 11px status dot, name at 590, 12.5px preview (own messages prefixed "Vos:"), tabular time on the right
- Selected = Selected Wash; hover = Hover Wash; offline dims the name and grays the dot
- Entrance: 0.5s ease-out rise with 45ms stagger on first load (the page's one authored moment)

### Message Bubbles
- Incoming lavender left / outgoing cream right, max `min(560px, 78%)`
- 10px between messages, 3px inside a group (same author, <5min)
- Meta lives inside: 11px tabular time; sent adds a 12px check; failed outlines the bubble in Danger and the whole bubble becomes a retry button

### Composer
- Sunken 14px-radius field with hairline stroke; focus-within brightens border and background
- Enter sends, Shift+Enter breaks line; auto-grows to 120px; a 10.5px counter appears past 460 characters

### Menus
- White 12px-radius popover, 220px min, floating shadow, 4px inner padding, 8px items
- Destructive items demand an inline confirmation step before acting ("Confirmar: …")

### Chips
- Status chips (Sunken Ground) and day chips (Hover Wash): 11–12px, 550 weight, pill radius; the demo badge uses the amber pair

## Do's and Don'ts

### Do:
- **Do** bundle every font and icon; the app must render with the network cable unplugged.
- **Do** keep metadata ≥4.5:1 on colored surfaces (#666A71 is the floor on both pastels).
- **Do** use tabular numerals for every time and count.
- **Do** pair every status dot with its word ("En línea", "Desconectado").
- **Do** let one animation speak: rows entering the list. Everything else transitions in ≤0.24s.

### Don't:
- **Don't** invent affordances the protocol can't keep: no typing indicators, no search below ~9 devices, no manual-add buttons.
- **Don't** introduce a third bubble color or a second typeface.
- **Don't** use emoji or glyph characters as icons; icons are stroked SVG (lucide).
- **Don't** load anything from the network at runtime — no CDN fonts, no remote images.
- **Don't** put kickers, eyebrows, or hard offset shadows on any surface.
