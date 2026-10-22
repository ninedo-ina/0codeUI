"""与后端 `server/src/model/prompt.rs` 对齐的 Prompt 约定。

训练语料的 system prompt 必须与线上推理完全一致，否则微调学到的
「输出单文件 HTML」约束会在上线后失效。`tests/test_corpus.py` 会直接读取
Rust 源文件并逐字比较，任何一侧改动都会让测试失败。
"""

from __future__ import annotations

import json
from typing import Any, Mapping

# 与 server/src/model/prompt.rs::SYSTEM_PROMPT 逐字一致。
SYSTEM_PROMPT = """你是 InTimeUI 的资深前端工程师，负责根据用户需求生成可直接在 WebView 中渲染的单文件 HTML 页面。

必须遵守：
1. 只输出一份完整的 HTML 文档，以 <!DOCTYPE html> 开头、</html> 结尾；不要输出 Markdown 代码围栏、解释或多余文字。
2. 所有 CSS 与 JavaScript 必须内联；禁止 <script src>、<link rel="stylesheet">、@import、外部字体 / 图片 / 接口资源。
3. 使用语义化 HTML（header/nav/main/section/article/footer 等），禁止无意义的 div 堆叠。
4. 视觉采用黑白灰主题，支持 light/dark 切换，切换按钮固定在右上角 Header；使用 CSS 自定义属性管理颜色与间距。
5. 响应式布局，移动端优先；交互元素必须有 hover / focus-visible / active 状态。
6. 动画只允许 transform / opacity / clip-path / filter，并尊重 prefers-reduced-motion。
7. 不嵌入任何密钥、token 或用户隐私数据；所有文本必须做 HTML 转义。
8. 页面结构完整、可直接渲染，不依赖任何构建步骤。"""


def canonical_json(value: Any) -> str:
    """稳定 JSON：对象键排序、紧凑分隔符、保留中文。"""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def build_user_prompt(sample: Mapping[str, Any]) -> str:
    """把语料样本拼成与线上 PromptAssembler 同构的用户消息。

    线上顺序：用户需求 → 场景编码 → 用户画像 → 业务数据。
    训练样本没有真实用户画像时，使用样本自带的风格字段（风格即目标偏好）。
    """
    lines = [
        f"用户需求：{sample['instruction']}",
        f"场景编码：{sample['scene_code']}",
    ]
    style = sample.get("style")
    if style:
        lines.append(f"用户界面偏好（JSON）：{canonical_json(style)}")
    business_data = sample.get("business_data")
    if business_data is not None:
        lines.append("业务数据（JSON）：")
        lines.append(canonical_json(business_data))
    return "\n".join(lines)
