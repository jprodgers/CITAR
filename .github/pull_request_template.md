## What this changes

<!-- One or two sentences. What is different after this is merged? -->

## Why

<!-- The problem being solved. If it fixes an issue, "Fixes #123". -->

## How it was checked

<!-- Delete what does not apply. -->

- [ ] `python -m unittest discover -s tests` passes
- [ ] `python scripts/audit_routes.py --strict` passes (needed if any HTTP route changed)
- [ ] `ruff check .` is clean
- [ ] Tried it in the browser
- [ ] New behaviour has a test

## Anything a reviewer should look at closely

<!-- A decision you were unsure about, a trade-off you made, something you could not test. -->
