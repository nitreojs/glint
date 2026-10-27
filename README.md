<p align="center"><picture><source media="(prefers-color-scheme: light)" srcset="docs/readme/hero-light.png"><img src="docs/readme/hero.png" alt="glint" width="880"></picture></p>

<p align="center">
screenshots and screen recording for windows, living in your tray<br>
<a href="https://github.com/nitreojs/glint/releases/latest"><b>download</b></a>
</p>

<br>

<picture><source media="(prefers-color-scheme: light)" srcset="docs/readme/head-screenshots-light.png"><img src="docs/readme/head-screenshots.png" alt="draw on it, type on it, blur the rest" width="880"></picture>

`ctrl+printscreen`, drag over the thing, done<br>
or don't stop there: pens, arrows, boxes, numbered steps, text in any font you have, blur and pixelate for the stuff nobody should see

<img src="docs/readme/1-screenshot.avif" alt="taking a screenshot and marking it up" width="880">

<br>

<picture><source media="(prefers-color-scheme: light)" srcset="docs/readme/head-grab-light.png"><img src="docs/readme/head-grab.png" alt="grab a piece, make it bigger" width="880"></picture>

grab any piece of the screen and drop it right on top, as big as you want<br>
paste pictures in with `ctrl+v`, pin the result above every window

<p align="center"><img src="docs/readme/2-grab.avif" alt="grabbing a piece of the screen and blowing it up" width="440"></p>

<br>

<picture><source media="(prefers-color-scheme: light)" srcset="docs/readme/head-recording-light.png"><img src="docs/readme/head-recording.png" alt="record it, then cut the boring part" width="880"></picture>

`ctrl+alt+printscreen`, pick a region or a window, go<br>
it records straight off the gpu with your mic and system sound, and can show your clicks and keys

then trim it, crop it, mute a track, squeeze it under a size<br>
if nothing has to be re-encoded, it isn't: the cut is lossless and glint says so

<img src="docs/readme/3-record.avif" alt="recording a region and trimming it" width="880">

<br>

<picture><source media="(prefers-color-scheme: light)" srcset="docs/readme/head-gallery-light.png"><img src="docs/readme/head-gallery.png" alt="everything, findable" width="880"></picture>

every shot and clip in one place<br>
hover to play, search by app or game or day, drag straight into a chat

<img src="docs/readme/4-gallery.avif" alt="the gallery" width="880">

<br>

<picture><source media="(prefers-color-scheme: light)" srcset="docs/readme/head-afterglow-light.png"><img src="docs/readme/head-afterglow.png" alt="it was recording the whole time" width="880"></picture>

**afterglow** is glint's little sibling for games: it keeps the last minute of your screen and sound, and `alt+f10` saves it<br>
clips open in glint's editor, and in deadlock it even knows when you got a kill (beta)

### get it

grab the exe from [releases](https://github.com/nitreojs/glint/releases/latest) and run it, it lands in the tray and updates itself from then on

<details>
<summary>building it yourself</summary>

```
cargo build --release -p glint
```

needs rust and windows, the exe ends up in `target/release/glint.exe`
</details>
