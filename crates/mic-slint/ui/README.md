# mic's window, in Slint

`app.slint` is the window: what the Rust side sets and the callbacks it answers (`src/main.rs`).
It's put together from:

| Where | What |
|---|---|
| `theme.slint` | `Theme`: the colours, timings and the emoji font |
| `types.slint` | `Tile` and `ChatLine`, the data Rust fills in |
| `widgets/` | What any screen uses: `Icon`, `Tip`, `RoundButton`, `PillButton`, `Banner`, `MenuRow`, `MenuPanel` |
| `room/` | The room's parts: `TileView`, `TileGrid`, `ChatPanel`, `Bar`, `ReactionPicker`, `MoreMenu` |
| `pages/` | `JoinScreen` and `RoomScreen` |
| `overlays/` | Over the window while they ask: `SharePicker`, `LinkQuestion` |

Every component is exported, so Slint's preview and inspector (the LSP's live preview, or
`slint-viewer`) open each on its own; those that want sample data have a `…Preview` beside them,
as `RoomPreview` in `pages/room.slint`:

```sh
slint-viewer crates/mic-slint/ui/pages/room.slint --component RoomPreview
slint-viewer crates/mic-slint/ui/room/tile.slint --component TilesPreview --screenshot tiles.png
```

`tests/ui.rs` loads every file alone and fails on a component left unexported. The previews set
the dark palette themselves, as `AppWindow` does. The software renderer, as `slint-viewer
--screenshot` uses, draws no colour emoji: the reactions show blank there.

An element that animates in sets its flag from a short `Timer`, not from `init`: a change made in
`init` lands before the first frame, and isn't animated. `--screenshot` renders that first frame,
so what fades in (a tile, the chat, a menu) is blank in it; the live preview runs the timers.
