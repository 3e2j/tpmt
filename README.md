<div align="center">

# Twilight Princess Modding Toolkit

</div>

> [!IMPORTANT]
> The project is being rebuilt as the [Arbiter](https://github.com/3e2j/Arbiter) game engine.
>
> This repository is no longer maintained.

## What is this?
This is legacy code for **TPMT (Twilight Princess Modding Toolkit)**.
See [this README](README-original.md) for the original information about the project.

TPMT is kept around for reference, and as a record of the project's history.

## Why legacy?
TPMT went through frequent scope changes, and defining a clear goalpost became extremely difficult.
As the project changed, early architectural decisions became difficult to justify. So a blank slate
was needed.

### Project history

#### 2023

Early on, I made a simple [BMS -> MIDI converter](https://github.com/3e2j/BMS-Analyzer). This was done as
a hobby reverse-engineering project to see if I could edit the binary music sequences (BMS) in Twilight Princess.
It was quite fun to implement, but it had limited use-cases, and was missing many features and/or was
incorrect in many places.

#### 2024
I wanted to remake some Zelda boss battles to learn how game engines work. Much inspired by videos like this 
[koloktos battle recreation](https://www.youtube.com/watch?v=B6yw3CaeDmo) which showed it could be feasible.
I tried my hand at this - but I had to download ~7 different tools to get models working.

Even after the fact, the moment I imported these models to [Blender](https://www.blender.org/) I encountered
issues with the scaling of the models, and had to make custom scripts to translate them and edit them. This was
far too much work for what it was worth. Separate tools were used for separate jobs, to unwrap, unpack, translate,
and edit. And most of these tools only did half a job. I ditched this venture out of tool-based frustration.

#### 2025
I regained fascination of format game-logic by watching speedruns and learning subtle quirks of the engine.
I have [bewildebeest](https://www.youtube.com/@bewildebeest) to thank for that. But it reignited the idea of if
there was "one tool that did everything", and didn't skimp on any features.

#### Mid 2026
TP was [fully decompiled](https://github.com/zeldaret/tp), and [dusklight released](https://twilitrealm.dev/).
It became possible to build tools with the actual game as reference, and with the games code available, the modding
scene quickly began to pickup. The initial project idea was now entirely achievable.

I began work on a TPMT prototype with a simple message decoder. It had only two functions: `unpack` and `build`.
A user would unpack the game, edit files in JSON, and build them back.

This worked great as a proof-of-concept! But it exposed the limitations of treating game files in isolation.
Format converters miss their mark because they don't understand game-specifics or interact with **the game** itself.

The project went through several scope changes:

1. A JSON editor with fixed choices (from game-tables).
2. A comprehensive format editor with project management.
3. A modding interface (with exports to platforms like [Dusklight](https://twilitrealm.dev/)).
4. An entire game engine for modding Twilight Princess.

As the scope changed, early decisions became increasingly harder to work with/around. Everything was wiped to
a blank slate.

## So what now?
I'm rebuilding the project from the ground up with a clear scope as [Arbiter](https://github.com/3e2j/Arbiter).

**A editor for viewing, editing, and building mods for Nintendo's GameCube and Wii games.**

This is starting with Twilight Princess support and extending to the first-party titles that share its systems (JSystem and friends).

The primary goal is still to unify tools under one roof. Rather than treating formats in isolation, the engine
should be able to understand how they fit together as a whole.

The engine will provide the ability to:
- Unpack game content
- Inspect, edit, and create assets for the game
- Build and package mods
- Launch builds to test them (in emulators like Dolphin or Dusklight)

### Future aspirations
The long-term goal is to **ship a runtime** alongside the editor. Making it a full-fledged game-engine.
At that point the project will no longer be limited to just modding. But using the same systems to develop
entirely new games.
