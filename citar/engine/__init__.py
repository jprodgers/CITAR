"""The game. Rules, state and the operations that change it — with no I/O anywhere.

Nothing in this package reads a file at runtime, opens a socket, touches the database or knows what
an HTTP request is. It takes a game state and an action and returns a new state or an error. The
one exception is the ruleset, which is loaded once at import and is read-only from then on.

That constraint is the reason the rest of CITAR works the way it does:

* A game is a **value**. Saving it is serialisation, loading it is the reverse, and the replay is a
  list of them. Nothing has to be reconstructed.
* A human in a browser, a scripted bot, a language model and an external agent are all *callers*,
  none of them privileged. There is one set of actions, in :mod:`citar.engine.tools`, so no
  interface can do something another cannot — and a model cannot cheat, because there is no move
  available to it that is not available to you.
* Tests are fast and deterministic. The suite plays real games in seconds with no fixtures, no
  database and no network.

Where to start
--------------
:mod:`citar.engine.state`    the data: ``GameState``, ``Player``, ``City``, ``Unit``, ``Tile``
:mod:`citar.engine.game`     ``Game``: the state plus everything that can be done to it
:mod:`citar.engine.tools`    the registry of player actions, and therefore the whole interface
:mod:`citar.engine.rules`    the ruleset, loaded from ``citar/data``
:mod:`citar.engine.uniques`  the interpreter that turns UnCiv rule text into behaviour

Rules are data. Most of what a unit or building does is expressed as *uniques* — short sentences
such as ``[+1 Science] per [2] population [in this city]`` — which :mod:`citar.engine.uniques`
parses and the systems consult. Adding content is usually a JSON entry rather than code; see
``docs/MODDING.md``.
"""
