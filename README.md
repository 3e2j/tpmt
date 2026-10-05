<div align="center">

# Twilight Princess Modding Toolkit

</div>

TPMT is a modding toolkit for *The Legend of Zelda: Twilight Princess* (GameCube).
Unpack the game's files, edit them with dedicated editors for each format, and build your changes back into the game.

## Why?
Existing tools for modding the game are few and far between, and most were built for a single use-case.
Because of this, many features are missing and compatibility is not guaranteed.

TPMT aims for:
- **Completeness**: Every field of every format.
- **Flexibility**: Edit with TPMT or your own tools.
- **Performance**: Builds only rebuild what you edited.
- **Compatibility**: Builds for disc, emulators, and mod-loaders (like [Dusklight](https://github.com/TwilitRealm/dusklight)).

> [!IMPORTANT]
> TPMT is in early development. Don't expect anything to work properly yet.

## Supported versions
The GameCube USA, PAL and JPN discs, as `.iso` or `.ciso`. Wii discs aren't supported yet.

## Supported formats
- **Disc**
  - [x] ISO and CISO disc images
  - [ ] DOL executable, apploader
  - [ ] BNR banner
- **Containers**
  - [x] RARC archives (`.arc`)
  - [x] Yaz0 compression
  - [x] Yay0 compression (optional)
- **Text**
  - [ ] BMG messages and dialogue flow (W.I.P)
- **Models**
  - [ ] BMD, BDL models
- **Animation**
  - [ ] BCK bone animation
  - [ ] BTK texture scrolling
  - [ ] BTP texture swapping
  - [ ] BRK, BPK colour animation
  - [ ] BLK, BLS facial animation
- **Textures and fonts**
  - [ ] BTI textures
  - [ ] BFN fonts
- **UI**
  - [ ] BLO screen layouts
- **Effects**
  - [ ] JPC particles
- **Cutscenes**
  - [ ] STB cutscene scripts
- **Stages**
  - [ ] DZS, DZR stage and room data
  - [ ] DZB, KCL collision
  - [ ] PLC collision attributes
- **Audio**
  - [ ] BAA audio archive
  - [ ] AW wave banks
  - [ ] BMS sequences
  - [ ] AST streamed music
  - [ ] BCT, CSW controller speaker sounds
- **Code**
  - [ ] REL modules, MAP symbol maps, STR
- **Video**
  - [ ] THP movies

## How to run
> [!IMPORTANT]
> You must provide your own game copy. This repository does *not* contain game assets.

To compile TPMT, you will need [Rust](https://www.rust-lang.org/tools/install) installed.

```sh
git clone https://github.com/3e2j/tpmt
cargo install --path tpmt/crates/tpmt
tpmt new game.iso        # unpack into ./game
cd game                  # copy files from base/ into mod/changes/ and edit them
tpmt status              # list edited files
tpmt build image         # or: patch, dusk
```

## Credits
Names and game structures come from [Dusklight](https://github.com/TwilitRealm/dusklight) and the
[Twilight Princess decompilation](https://github.com/zeldaret/tp).

## License
[CC0-1.0](LICENSE.md)
