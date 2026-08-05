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
2. **Unpack** it with the toolkit. You get a project folder holding the game's files, decoded into formats you can actually edit.
3. **Edit** anything you want, in place. Use the toolkit's tools, or your own.
4. **Build.** The toolkit works out which files you changed and packs them back into a mod others can install (`tpmt build`), or patches it straight into a playable ISO (`tpmt image`).

Your original ISO is never modified. The toolkit remembers what files you've modified, and can be checked with `tpmt status` to tell what files have changed. Building only compiles the files you've edited, unedited files are copied verbatim from the original source.

> [!IMPORTANT]
> You must provide your own game copy. This repository does *not* contain game assets.
