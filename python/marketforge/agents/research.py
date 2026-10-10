"""Public-web research with pinned public IPs, bounded responses and source receipts."""
import datetime
import hashlib
import http.client
import ipaddress
import json
import os
import socket
import ssl
from html.parser import HTMLParser
from urllib.parse import quote, urljoin, urlsplit
from xml.etree import ElementTree


def public_addresses(host, port):
    addresses = sorted({entry[4][0] for entry in socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)})
    if not addresses or any(not ipaddress.ip_address(address).is_global or ipaddress.ip_address(address).is_multicast for address in addresses):
        raise ValueError("research tools access public internet addresses only")
    return addresses


class TextPage(HTMLParser):
    def __init__(self, base):
        super().__init__(convert_charrefs=True)
        self.base, self.parts, self.links, self.hidden = base, [], [], 0
    def handle_starttag(self, tag, attrs):
        if tag in ("script", "style", "noscript"):
            self.hidden += 1
        if tag == "a" and len(self.links) < 100:
            href = dict(attrs).get("href", "")
            if href:
                url = urljoin(self.base, href)
                if urlsplit(url).scheme in ("http", "https"):
                    self.links.append(url)
    def handle_endtag(self, tag):
        if tag in ("script", "style", "noscript"):
            self.hidden = max(0, self.hidden - 1)
        if tag in ("p", "div", "br", "li", "h1", "h2", "h3"):
            self.parts.append("\n")
    def handle_data(self, data):
        if not self.hidden:
            self.parts.append(data)


class Research:
    def fetch(self, url):
        original = url
        for _ in range(5):
            parsed = urlsplit(url)
            if (parsed.scheme not in ("http", "https") or not parsed.hostname or parsed.username or parsed.password
                    or len(url) > 4096 or any(ord(c) < 32 for c in url)):
                raise ValueError("expected a public HTTP(S) URL without credentials")
            port = parsed.port or (443 if parsed.scheme == "https" else 80)
            if port not in (80, 443):
                raise ValueError("research allows standard web ports only")
            # Connect to the validated address directly: no DNS rebinding window.
            address = public_addresses(parsed.hostname, port)[0]
            connection = http.client.HTTPConnection(parsed.hostname, port, timeout=15)
            raw = socket.create_connection((address, port), timeout=15)
            try:
                connection.sock = ssl.create_default_context().wrap_socket(raw, server_hostname=parsed.hostname) if parsed.scheme == "https" else raw
                path = quote(parsed.path or "/", safe="/%:@!$&'()*+,;=-._~")
                if parsed.query:
                    path += "?" + quote(parsed.query, safe="%=&+:;/,?@!$'()*-._~")
                connection.request("GET", path, headers={"User-Agent": "MarketForge-Research/1.0", "Accept-Encoding": "identity", "Accept": "text/html,application/json,application/rss+xml,text/plain,*/*;q=0.1"})
                response = connection.getresponse()
                if response.status in (301, 302, 303, 307, 308):
                    url = urljoin(url, response.getheader("Location", ""))
                    continue
                if response.status != 200:
                    raise ValueError(f"web source returned HTTP {response.status}")
                data = response.read(1_048_577)
                if len(data) > 1_048_576:
                    raise ValueError("web response exceeds 1 MiB; choose a smaller page/feed")
                mime = response.getheader("Content-Type", "text/plain").lower()
                if not any(kind in mime for kind in ("text/", "json", "xml")):
                    raise ValueError("use an HTML, text, JSON or RSS source")
                return {"url": url, "requested_url": original, "retrieved_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                        "sha256": hashlib.sha256(data).hexdigest(), "content_type": mime, "raw": data.decode("utf-8", errors="replace")}
            finally:
                connection.close()
                raw.close()
        raise ValueError("too many web redirects")

    def read(self, url):
        result = self.fetch(url)
        content = result.pop("raw")
        if "html" in result["content_type"]:
            page = TextPage(result["url"]); page.feed(content)
            content = " ".join(page.parts)
            result["links"] = list(dict.fromkeys(page.links))
        result.update(text=content[:24000], truncated=len(content) > 24000, untrusted_external_data=True)
        return result

    def search(self, query):
        if not isinstance(query, str) or not 1 <= len(query) <= 500:
            raise ValueError("search query must have 1-500 characters")
        template = os.environ.get("MARKETFORGE_AGENT_SEARCH_URL", "https://www.bing.com/search?format=rss&q={query}")
        result = self.fetch(template.replace("{query}", quote(query)))
        raw = result.pop("raw")
        if "json" in result["content_type"]:
            rows = json.loads(raw).get("results", [])
            items = [{"title": r.get("title", ""), "url": r.get("url", ""), "snippet": r.get("content", "")} for r in rows[:10]]
        else:
            try:
                tree = ElementTree.fromstring(raw)
            except ElementTree.ParseError as exc:
                raise ValueError("search provider did not return RSS/JSON; use web_read with a source URL or configure another search provider") from exc
            items = [{"title": i.findtext("title", ""), "url": i.findtext("link", ""), "snippet": i.findtext("description", "")} for i in tree.findall(".//item")[:10]]
        result.update(query=query, results=items, untrusted_external_data=True)
        return result
