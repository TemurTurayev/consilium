# Newbie-UX + Installer Design

**Date:** 2026-06-23  
**Branch:** newbie-ux  
**Status:** In progress

## Goal

A beginner should be able to install Consilium and immediately understand what to do — like Claude Code's CLI, unlike Cursor. The friction from "just installed it" to "running my first task" must be near zero, with no jargon walls.

## Changes in this slice

### 1. Bare `consilium` prints a warm welcome (not a clap usage error)

Make `command: Command` optional (`Option<Command>`). When no subcommand is given, print a friendly welcome that names the two most important first steps (`init`, then `conduct`) and a short inventory of other commands. Exit 0.

### 2. `consilium init` wizard: explain roles in plain English

The wizard currently prints `role  provider/model` with no description. A newbie does not know what a "conductor" is. After the lineup preview, print a short legend mapping each role to a plain-English job description:

- conductor — plans the work and reviews every change (your smartest model)
- chairman — makes the final call when the models disagree
- worker — writes the actual code (cheaper, faster models)
- reviewer — double-checks each change for bugs
- supervisor — watches the whole run and flags trouble

Replace the wizard's closing lines with a warmer, concrete closing that tells the user exactly what to type next.

### 3. `init --yes` (non-interactive) also points to the next step

Append a first-task hint so CI/script users see the same "what now" guidance as interactive users.

### 4. Friendlier "no providers" guidance in conduct/auto

When the preflight fails (conductor has no reachable model), augment the error to also suggest running `consilium init` from scratch, not just doctor.

## Follow-up slice: installer

The **installer** (curl|sh one-liner + Homebrew tap via cargo-dist) is intentionally out of scope here. It warrants its own design doc covering:

- cargo-dist configuration for macOS/Linux/Windows targets
- Homebrew tap repo and formula structure
- Install-to-first-run flow (PATH check, shell completion, post-install message)
- CI/CD release pipeline (tag → GitHub Release → tap auto-PR)

This will be addressed in a follow-up `installer` branch.
