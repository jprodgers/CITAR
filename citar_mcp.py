"""Entry point for MCP clients: python citar_mcp.py --server http://127.0.0.1:8765 --game <id> --token <seat token>"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from citar.agents.mcp_server import main

if __name__ == "__main__":
    main()
