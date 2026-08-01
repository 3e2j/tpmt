# Twilight Princess Modding Toolkit

TPMT is a modding toolchain for *The Legend of Zelda: Twilight Princess* (GameCube). 
It provides a unified toolset for unpacking, editing, and building files into the game.

> ⚠️ In development - do not expect anything to work properly at this stage.

## Why?
Currently the tools available for modding the game are few-and-far between, many of which are custom built for a specific use-case. 
Because of this, many implementations cut corners and are missing crucial features. 

This project attempts to provide an updated and complete toolkit, capturing all relevant format features.

## How it works
1. **Provide** an ISO copy of the game.
2. **Unpack** it with the toolkit. You get a full copy of the game's files to work with.
3. **Edit** anything you want, in place. Use the toolkit's tools, or your own.
4. **Build.** The toolkit works out which files you changed and packs them back into a playable ISO.

Your original ISO is never modified, the toolkit keeps a clean copy of the game to compare against. Run `tpmt status` at any point to see the list of what your mod changed.

> [!IMPORTANT]
> You must provide your own game copy. This repository does *not* contain game assets.
