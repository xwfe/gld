"""假的构建：只负责吐出一份比一次调用能带回去的多得多的日志。"""

from __future__ import annotations

for step in range(1, 401):
    print(f"[build] step {step:03d}/400 compiling unit {step * 7 % 97:02d}")
print("build ok")
