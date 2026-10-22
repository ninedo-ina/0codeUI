"""评测用 provider：确定性 mock 与 OpenAI 兼容推理服务。

mock 只用于无 GPU 环境验证「评测链路」；真实质量结论必须用
`--provider openai` 指向 vLLM 的 base model / LoRA adapter。
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from decimal import Decimal
from html import escape
from pathlib import Path
from string import Template
from typing import Any, Mapping

TEMPLATES_DIR = Path(__file__).resolve().parents[1] / "corpus" / "templates"

STATUS_LABELS = {
    "PENDING": "待支付",
    "PAID": "已支付",
    "SHIPPED": "已发货",
    "COMPLETED": "已完成",
    "CANCELLED": "已取消",
}


def _money(value: Any) -> str:
    try:
        return f"¥{Decimal(str(value)).quantize(Decimal('0.01'))}"
    except Exception:
        return "¥0.00"


def _render_order_detail(business_data: Mapping[str, Any], theme: str) -> str:
    items = business_data.get("items") or []
    rows: list[str] = []
    total = Decimal("0")
    total_qty = 0
    for item in items:
        price = Decimal(str(item.get("price", 0)))
        qty = int(item.get("qty", 0))
        subtotal = price * qty
        total += subtotal
        total_qty += qty
        rows.append(
            "<tr>"
            f"<td>{escape(str(item.get('name', '')))}</td>"
            f"<td>{_money(price)}</td>"
            f"<td class=\"amount\">{qty}</td>"
            f"<td class=\"amount\">{_money(subtotal)}</td>"
            "</tr>"
        )
    template = Template((TEMPLATES_DIR / "order_detail.html").read_text(encoding="utf-8"))
    return template.safe_substitute(
        THEME=theme,
        ORDER_ID=escape(str(business_data.get("order_id", ""))),
        STATUS=escape(str(business_data.get("status", "PENDING"))),
        STATUS_LABEL=STATUS_LABELS.get(str(business_data.get("status")), "未知状态"),
        ITEM_COUNT=len(items),
        TOTAL_QTY=total_qty,
        ITEMS_ROWS="\n          ".join(rows),
        TOTAL=_money(total),
    )


def _render_order_list(business_data: Mapping[str, Any], theme: str) -> str:
    orders = business_data.get("orders") or []
    rows: list[str] = []
    for order in orders:
        status = str(order.get("status", "PENDING"))
        rows.append(
            "<li class=\"order\">"
            "<div class=\"order-main\">"
            "<span class=\"meta-label\">订单号</span>"
            f"<span class=\"order-id\">{escape(str(order.get('order_id', '')))}</span>"
            f"<span class=\"order-meta\">{escape(str(order.get('created_at', '')))} · "
            f"{int(order.get('item_count', 0))} 件商品</span>"
            "</div>"
            "<div class=\"order-side\">"
            "<span class=\"meta-label\">订单状态</span>"
            f"<span class=\"chip\">{STATUS_LABELS.get(status, '未知状态')}</span>"
            "<span class=\"meta-label\">金额</span>"
            f"<span class=\"amount\">{_money(order.get('total', 0))}</span>"
            "</div>"
            "</li>"
        )
    template = Template((TEMPLATES_DIR / "order_list.html").read_text(encoding="utf-8"))
    return template.safe_substitute(
        THEME=theme,
        ORDER_COUNT=len(orders),
        ORDER_ROWS="\n      ".join(rows),
    )


def _render_product_detail(business_data: Mapping[str, Any], theme: str) -> str:
    template = Template((TEMPLATES_DIR / "product_detail.html").read_text(encoding="utf-8"))
    return template.safe_substitute(
        THEME=theme,
        PRODUCT_NAME=escape(str(business_data.get("name", ""))),
        PRODUCT_ID=escape(str(business_data.get("product_id", ""))),
        PRICE=_money(business_data.get("price", 0)),
        DESCRIPTION=escape(str(business_data.get("description", ""))),
        STOCK=escape(str(business_data.get("stock", "充足"))),
        SHIPPING=escape(str(business_data.get("shipping", "48 小时内"))),
    )


def render_template(scene: str, business_data: Mapping[str, Any], theme: str = "light") -> str:
    """按场景渲染内置模板（种子语料与 mock provider 共用）。"""
    if scene == "ORDER_LIST_PAGE":
        return _render_order_list(business_data, theme)
    if scene == "PRODUCT_DETAIL_PAGE":
        return _render_product_detail(business_data, theme)
    return _render_order_detail(business_data, theme)


class MockProvider:
    """按场景模板返回确定性 HTML，用于无 GPU 的链路自检。"""

    def describe(self) -> dict[str, Any]:
        return {
            "kind": "mock",
            "model": "intimeui-template-v1",
            "quality_claim": False,
            "note": "确定性模板输出，仅验证评测链路，不代表模型质量",
        }

    def generate(
        self, messages: list[dict[str, str]], max_tokens: int = 2048, temperature: float = 0.2
    ) -> str:
        del max_tokens, temperature
        user = next((item["content"] for item in reversed(messages) if item["role"] == "user"), "")
        scene = "ORDER_DETAIL_PAGE"
        if "场景编码：" in user:
            scene = user.rsplit("场景编码：", 1)[1].splitlines()[0].strip()
        payload = _extract_business_data(user)
        theme = "dark" if '"theme":"dark"' in user.replace(" ", "") else "light"
        return render_template(scene, payload, theme)


def _extract_business_data(user: str) -> dict[str, Any]:
    marker = "业务数据（JSON）："
    if marker not in user:
        return {}
    payload = user.rsplit(marker, 1)[1].strip()
    try:
        value = json.loads(payload)
        return value if isinstance(value, dict) else {}
    except json.JSONDecodeError:
        return {}


class OpenAICompatibleProvider:
    """调用 vLLM / Ollama / DeepSeek 的 `/chat/completions`。"""

    def __init__(
        self,
        base_url: str,
        model: str,
        api_key: str | None = None,
        timeout: float = 120.0,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.model = model
        self.api_key = api_key or ""
        self.timeout = timeout

    def _endpoint(self) -> str:
        if self.base_url.endswith("/chat/completions"):
            return self.base_url
        return f"{self.base_url}/chat/completions"

    def describe(self) -> dict[str, Any]:
        return {
            "kind": "openai_compatible",
            "model": self.model,
            "base_url": self.base_url,
            "quality_claim": True,
        }

    def generate(
        self, messages: list[dict[str, str]], max_tokens: int = 2048, temperature: float = 0.2
    ) -> str:
        payload = {
            "model": self.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "temperature": temperature,
            "stream": False,
        }
        request = urllib.request.Request(
            self._endpoint(),
            data=json.dumps(payload, ensure_ascii=False).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        if self.api_key:
            request.add_header("Authorization", f"Bearer {self.api_key}")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                body = response.read().decode("utf-8")
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", errors="replace")[:500]
            raise RuntimeError(f"upstream HTTP {exc.code}: {detail}") from exc
        except urllib.error.URLError as exc:
            raise RuntimeError(f"upstream unreachable: {exc.reason}") from exc
        parsed = json.loads(body)
        choices = parsed.get("choices") or []
        if not choices:
            raise RuntimeError("upstream returned no choices")
        content = choices[0].get("message", {}).get("content", "")
        if isinstance(content, list):
            content = "".join(
                part.get("text", "") for part in content if isinstance(part, dict)
            )
        if not isinstance(content, str) or not content.strip():
            raise RuntimeError("upstream returned empty content")
        return content
