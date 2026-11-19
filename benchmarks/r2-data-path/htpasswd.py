import ctypes
import ctypes.util
import os
import subprocess
from pathlib import Path

USER = "bench"
PASSWORD = "benchpass"


def apr1_hash():
    return subprocess.run(["openssl", "passwd", "-apr1", PASSWORD], check=True, capture_output=True, text=True).stdout.strip()


def bcrypt_hash(cost):
    libcrypt = ctypes.CDLL(ctypes.util.find_library("crypt"))
    libcrypt.crypt_gensalt.restype = libcrypt.crypt.restype = ctypes.c_char_p
    salt = libcrypt.crypt_gensalt(b"$2y$", cost, os.urandom(16), 16)
    return libcrypt.crypt(PASSWORD.encode(), salt).decode()


def write(path: Path, bcrypt_cost=None):
    path.write_text(f"{USER}:{apr1_hash() if bcrypt_cost is None else bcrypt_hash(bcrypt_cost)}\n")
    path.chmod(0o600)
