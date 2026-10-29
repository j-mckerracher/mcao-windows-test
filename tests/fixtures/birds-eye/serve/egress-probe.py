#!/usr/bin/env python3
"""TCP connect probe for the network-isolation proof (DPES-798, AC7/T12).

Usage: egress-probe.py <host> <port>

Attempts one TCP connect with the standard library only and sends no payload. Prints
"connect ok host=<h> port=<p>" and exits 0 on success. On failure prints
"connect failed errno=<n> <ExceptionType>" and exits 3, so a sandbox denial (errno 1,
PermissionError) is distinguishable from an unreachable host.
"""
import socket
import sys

host, port = sys.argv[1], int(sys.argv[2])
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.settimeout(5)
try:
    sock.connect((host, port))
except OSError as exc:
    print(f'connect failed errno={exc.errno} {type(exc).__name__}')
    sys.exit(3)
finally:
    sock.close()
print(f'connect ok host={host} port={port}')
sys.exit(0)
