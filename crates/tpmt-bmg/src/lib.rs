//! BMG binary message files.
//!
//! Every line of dialogue, item name, and sign in the game. A BMG is a pool of
//! Shift-JIS strings alongside a table of fixed-width attribute records, one
//! per message, whose field layout varies from file to file. The strings may carry
//! inline tags for colour, ruby text, button glyphs, and control flow.
//!
//! Additionally, BMGs also allow for connecting a flow graph between message nodes,
//! allowing for messages to flow seemlessly between each other. This flow may
//! branch based on conditions, or emit events which do certain actions (such as
//! giving the player an item, setting flags, or triggering screen effects).
