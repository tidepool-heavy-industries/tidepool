import os, socket, signal
if os.fork() == 0:
    os.setsid()
    if os.fork() != 0:
        os._exit(0)
    os.write(1, b"x" * (1024 * 1024))
    s = socket.socket(socket.AF_UNIX)
    s.connect("ready")
    # The socket remains held by the detached descendant until namespace drain.
    s.sendall(b"ready\n")
    signal.pause()
signal.pause()
