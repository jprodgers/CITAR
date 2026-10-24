"""CPU jobs on the server (design/phase4-helper.md P4.4, P4.5).

- :mod:`citar.jobs.host`: the server's job host, from the parent's side: ``python -m citar.jobs host`` started as a
  child under the limits of the host machine's row, its ``ready`` held to the extension's jobs build, its frames, and
  why it died (a memory death fails every run of it as ``memory``). The job host itself is the jobs crate's, the same
  Rust as the helper's ``citar-helper job-host`` (``citar_jobs::host``).

The dispatcher, the store and the local executor that run the lab's games through it are package 4-09's.
"""
