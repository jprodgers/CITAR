"""``python -m citar.jobs host``: the server's job host (design/phase4-helper.md P4.4.3).

It reads the job host's frames as JSON lines on its standard input and writes its own on its standard output, under the
limits its first frame gives, until its input closes (``citar.engine_api.run_job_host``). Nothing else may write to
its standard output: it is the protocol. :mod:`citar.jobs.host` starts it; a person never needs to.
"""
import sys


def main(argv: list) -> int:
    if argv != ["host"]:
        print("usage: python -m citar.jobs host   (the server's job host: started by the server, not by hand)",
              file=sys.stderr)
        return 2
    from citar import engine_api
    return engine_api.run_job_host()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
