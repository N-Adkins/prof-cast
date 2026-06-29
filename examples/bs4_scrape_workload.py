"""
A CPU-bound BeautifulSoup workload to profile.

Builds a large synthetic HTML document, then repeatedly parses it and runs the
kind of navigation/search a scraper does (selects, attribute filtering, text
extraction).
"""

import time

from bs4 import BeautifulSoup


def build_html(rows=400):
    parts = ["<html><head><title>Catalog</title></head><body>"]
    parts.append("<nav><ul>")
    for i in range(20):
        parts.append(f'<li class="menu-item"><a href="/cat/{i}">Category {i}</a></li>')
    parts.append("</ul></nav><main>")
    for r in range(rows):
        parts.append(
            f'<div class="product" data-id="{r}" data-price="{r * 3 % 97}">'
            f'<h2 class="name">Product {r}</h2>'
            f'<p class="desc">Lorem ipsum dolor sit amet number {r}, '
            f"consectetur adipiscing elit sed do eiusmod.</p>"
            f'<span class="tag">tag-{r % 13}</span>'
            f'<span class="tag">group-{r % 7}</span>'
            f"<table><tr><td>sku</td><td>SKU-{r:05d}</td></tr>"
            f"<tr><td>stock</td><td>{r * 7 % 200}</td></tr></table>"
            f"</div>"
        )
    parts.append("</main></body></html>")
    return "".join(parts)


def scrape(html):
    soup = BeautifulSoup(html, "html.parser")
    total = 0
    for prod in soup.find_all("div", class_="product"):
        name = prod.find("h2", class_="name").get_text()
        desc = prod.find("p", class_="desc").get_text()
        tags = [t.get_text() for t in prod.find_all("span", class_="tag")]
        price = int(prod.get("data-price", "0"))
        cells = [td.get_text() for td in prod.find_all("td")]
        total += len(name) + len(desc) + len(tags) + price + len(cells)
    links = [a.get("href") for a in soup.find_all("a")]
    return total + len(links)


def main(seconds=8.0):
    html = build_html()
    deadline = time.time() + seconds
    runs = 0
    acc = 0
    while time.time() < deadline:
        acc += scrape(html)
        runs += 1
    print(f"completed {runs} scrape passes, checksum={acc}")


if __name__ == "__main__":
    main()
