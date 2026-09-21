# Images

`icon.png` is generated — `python installer/make_icon.py` writes it and `installer/citar.ico` from
the same drawing. Do not edit it by hand; edit the script.

## Screenshots

There are none yet, and there should be. If you are adding some, these are the ones the
documentation would use, with the names it expects:

| File | What it should show |
|---|---|
| `game.png` | A mid-game map with cities, units and a unit panel open. The hero image |
| `lobby.png` | The lobby with a game being created, showing the seat types |
| `benchmarks.png` | A benchmark run in progress, with per-game progress and scores |
| `stats.png` | The AI stats screen, showing turn timing and how turns ended |
| `report.png` | A generated report, showing the charts |
| `welcome.png` | The first-run wizard having found a model server |

How to take them:

1. Run CITAR at a window width of about 1280 pixels. The client is dark by default, which is what
   the documentation assumes.
2. Load a game that has something in it — an empty map makes a dull screenshot and says nothing
   about what CITAR does.
3. Crop to the browser viewport. No window chrome, no bookmarks bar, no desktop.
4. Save as PNG, under about 400 KB each. They go in the repository, and a megabyte of screenshot
   per page is felt by everyone who clones it.

Check before committing that there is nothing personal in them: real hostnames in the Servers page,
an e-mail address in the account menu, a path with your user name in it, a browser tab from
something else.

Once they exist, add them to `README.md` (a hero image under the title) and to the page each one
belongs to.
