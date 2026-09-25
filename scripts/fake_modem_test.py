#!/usr/bin/env python3
"""pty で偽モデムを作り、null-bbs のモデム回線を通しで試す。

  python3 scripts/fake_modem_test.py target/debug/null-bbs

偽モデムは AT コマンドに OK を返し、RING → ATA → CONNECT の着信を再現する。
接続後は利用者としてログインし、NO CARRIER によるキャリア断と、切断後の再初期化を確かめる。
"""
import os, pty, re, subprocess, sys, tempfile, time, tty

BIN = os.path.abspath(sys.argv[1])
master, slave = pty.openpty(); tty.setraw(slave)
work = tempfile.mkdtemp(prefix="nbbs-modem-")
open(f"{work}/null-bbs.toml", "w").write(f"""
[bbs]
name = "MODEM-TEST"
max_lines = 2
[tcp]
listen = []
[[modem]]
line = 1
path = "{os.ttyname(slave)}"
init = ["ATZ", "ATE0V1Q0X4&C1&D2S0=0"]
carrier = "text"
hangup = "escape"
connect_delay_ms = 200
""")
proc = subprocess.Popen([BIN, "run", "--headless"], cwd=work, stderr=open(f"{work}/stderr.log", "w"))
buf = b""

def expect(pat, timeout=10):
    global buf
    end = time.time() + timeout
    while time.time() < end:
        m = re.search(pat.encode(), buf)
        if m:
            got = buf[:m.end()]; buf = buf[m.end():]
            return got.decode("utf-8", "replace")
        r = os.read(master, 4096) if select_ready(0.2) else b""
        buf += r
    raise AssertionError(f"'{pat}' が来ない。受信: {buf[-300:]!r}")

def select_ready(t):
    import select
    return bool(select.select([master], [], [], t)[0])

def send(s): os.write(master, s.encode() if isinstance(s, str) else s)
def modem_ok_until_idle():
    """初期化コマンドに OK を返す"""
    for cmd in ["ATZ", "ATE0V1Q0X4&C1&D2S0=0"]:
        expect(re.escape(cmd) + "\r"); send("\r\nOK\r\n")

try:
    modem_ok_until_idle(); print("初期化: OK")
    time.sleep(0.5); send("\r\nRING\r\n"); expect("ATA\r"); send("\r\nCONNECT 2400/V42BIS\r\n"); print("着信応答: OK")
    expect("ID を入力"); send("NEW\r"); expect("希望する ID"); send("modemuser\r"); expect("ハンドル"); send("モデム太郎\r")
    expect("パスワード"); send("abcd1234\r"); expect("もう一度"); send("abcd1234\r"); expect("よろしいですか"); send("Y\r")
    expect("コマンド"); send("WHO\r"); t = expect("コマンド"); assert "2400/V42BIS" in t, t; print("ログイン・WHO (速度表示): OK")
    # キャリア断
    send("\r\nNO CARRIER\r\n"); expect(r"\+\+\+", 8); expect("ATH0\r", 5); send("\r\nOK\r\n"); print("キャリア断 → +++ATH0: OK")
    modem_ok_until_idle(); print("再初期化: OK")
    # 2 回目の着信とログオフ
    time.sleep(0.5); send("\r\nRING\r\n"); expect("ATA\r"); send("\r\nCONNECT 1200\r\n")
    expect("ID を入力"); send("GUEST\r"); expect("コマンド"); send("BYE\r"); expect("ご利用ありがとう")
    expect(r"\+\+\+", 8); expect("ATH0\r", 5); send("\r\nOK\r\n"); modem_ok_until_idle(); print("ログオフ → 切断 → 再初期化: OK")
    print("ALL OK")
finally:
    proc.terminate(); proc.wait()
    print(open(f"{work}/stderr.log").read()[-1200:])
