"""InTimeUI UI 语料与 LoRA/QLoRA 微调管线的公共库。

模块只依赖 Python 标准库，训练脚本再按需导入 torch / transformers / peft，
这样语料清洗、评测与清单校验可以在无 GPU、无深度学习依赖的 CI 上运行。
"""

__version__ = "1.0.0"
