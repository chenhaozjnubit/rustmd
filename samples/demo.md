# rustmd 功能演示

这是一段普通正文，用来检验**行内加粗**、*斜体*、***粗斜体***、~~删除线~~、`行内代码` 与 ==高亮== 的渲染效果。
中文排版应当有合适的行高与字距，英文 mixing text 也要自然。

## 二级标题

### 三级标题
#### 四级标题

> 引用块：文字应当有左侧竖线，颜色略淡。
> 支持多行，也支持引用里的 **行内标记**。

---

## 列表

- 无序项一
- 无序项二
  - 嵌套项 A
  - 嵌套项 B
- 无序项三

1. 有序项
2. 第二项
3. 第三项

- [x] 已完成的任务
- [ ] 未完成的任务

## 代码

```rust
use std::collections::HashMap;

/// 计算斐波那契数列
fn fib(n: u64) -> u64 {
    let mut memo: HashMap<u64, u64> = HashMap::new();
    (0..=n).fold((0, 1), |(a, b), i| {
        memo.insert(i, a);
        (b, a + b)
    }).1
}
```

```python
def greet(name: str) -> str:
    return f"hello, {name}"
```

## 表格

| 功能 | 状态 | 说明 |
| --- | :---: | ---: |
| 实时渲染 | 完成 | 光标所在块显示源码 |
| 数学公式 | 完成 | 自研排版引擎 |
| 导出 HTML | 完成 | 单文件、带样式 |

## 数学

行内公式 $E = mc^2$，以及 $\alpha + \beta = \gamma$。

$$
\sum_{i=1}^{n} x_i = \frac{n(n+1)}{2}
$$

$$
\int_{0}^{\infty} e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}
$$

$$
\left( \frac{a}{b} \right)^{2} \qquad \sqrt[3]{x + y} \qquad \lim_{x \to 0} \frac{\sin x}{x} = 1
$$

## 链接与图片

[Anthropic](https://www.anthropic.com) 与 <https://www.rust-lang.org>。

## 脚注与杂项

这是一段带脚注的文字[^1]。

[^1]: 脚注内容在这里。

行尾两个空格换行  
第二行。
