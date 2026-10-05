# Where is everything?
The dependency/caller structure works like this:

`Frontend` -> `Entry point` -> `Core` -> `Formats`

---

## Frontend
- `tpmt`: the CLI.

## Entry point (command center)
- `ops`: all operations a frontend calls (unpack, status, edit, build). It joins the core crates, which don't depend on each other.

## Core (called by the entry point)
- `core/project`: the project folder and its files, made initially from an unpack.
- `core/packing` ("packaging" logic): unpacking a disc into files and building it back.
- `core/editing` ("payloads" logic): logic for editing "payload" formats (a game-asset), saving edits as a patch against vanilla.

## Formats
  ### Utilities
  - `formats/binary`: big-endian reads and writes, and the `Format` trait every format implements.
  - `formats/report`: warnings and progress reports that are sent back to the caller.

  ### Format interpretation (the good stuff)
  > Encoding/decoding raw bytes of formats into structs
  - `formats/packaging/*`: Things that wrap a payload (disc images, archives, compression).
  - `formats/payloads/*`: Game-asset files, such as BMG messages.
  
  ### Game tables
  > These are game-specific implementations are not inferred by formats themselves.
  - `formats/tables`: names and definitions for the game's raw values.

## Tests
> Outside the general flow, only used for dev testing.
- `retail-tests`: checks the format crates against the retail discs in `discs/`.
