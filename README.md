# hyprselector

Select Hyprland windows using keyboard hints.

## Risks

- Only test it on my `Hyprland 0.56.2`.
- Hyprland with `hyprlang` configuration is Unsupported.

## Requirements

- Hyprland with a Lua configuration and `hyprctl` on your PATH;
- Rust toolchains;

## Compiling

Ensure `~/.cargo/bin` is on your PATH and Install the build dependencies:

```bash
# Arch Linux
sudo pacman -S --needed rust pkgconf libxkbcommon
```

From the repository root:

```bash
cargo run
```

Or, you could install the compiled binary using:

```
cargo run --path .
```

## Usage

Is acts like [tmux active pane changing](https://github.com/tmux/tmux/wiki/Getting-Started#changing-the-active-pane)

```bash
hyprselector
```

| Key | Action |
| --- | --- |
| Letters | Select a window as soon as its full label matches |
| Backspace | Delete the last letter |
| Esc | Cancel |

### Keybinding

Add a binding to your `hyprland.lua`:

```lua
hl.bind("SUPER + W", hl.dsp.exec_cmd("hyprselector"))
```

## License

[MIT](LICENSE).

The bundled DejaVu Sans Mono font is distributed under its [own license](assets/FONT-LICENSE.txt).
