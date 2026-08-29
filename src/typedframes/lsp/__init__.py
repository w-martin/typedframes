"""Editor-agnostic Language Server Protocol integration.

Requires the optional `pygls` dependency: `pip install typedframes[lsp]`. Run the
server as `typedframes-lsp` or `python -m typedframes.lsp`; see
`typedframes.lsp.server` for what it does and does not implement.
"""

from typedframes.lsp.server import create_server, main

__all__ = ["create_server", "main"]
