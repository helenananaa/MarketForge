"""Read native receipt fields/arrays without building a whole-file Python tree."""
import codecs
from collections.abc import Mapping
import json
import mmap
from pathlib import Path
import re

STRING = re.compile(rb'"(?:[^"\\]|\\.)*"')
STRUCTURE = re.compile(rb'"(?:[^"\\]|\\.)*"|[\[\]{}]')
SPACE = b' \t\r\n'
MAX_VALUE = 16*1024*1024


def _space(view, pos):
    while pos < len(view) and view[pos] in SPACE:pos += 1
    return pos


def _end(view, start):
    first=view[start]
    if first==34:
        match=STRING.match(view,start)
        if not match:raise ValueError('Unclosed JSON string')
        return match.end()
    if first in (91,123):
        stack=[]
        for token in STRUCTURE.finditer(view,start):
            kind=view[token.start()]
            if kind==34:continue
            if kind in (91,123):stack.append(kind)
            elif not stack or stack.pop()!={93:91,125:123}[kind]:
                raise ValueError('Mismatched JSON delimiters')
            if not stack:return token.end()
        raise ValueError('Unclosed JSON collection')
    pos=start
    while pos<len(view) and view[pos]not in b',} \t\r\n':pos+=1
    if pos==start:raise ValueError('Missing JSON value')
    return pos


class ArrayView:
    def __init__(self,path,start,end):self.path,self.start,self.end=path,start,end
    def __iter__(self):
        with self.path.open('rb')as stream:
            stream.seek(self.start+1)
            remaining=self.end-self.start-2
            utf8=codecs.getincrementaldecoder('utf-8')();decoder=json.JSONDecoder()
            buf='';pos=0;want_value=True;seen=False
            while True:
                while pos<len(buf)and buf[pos].isspace():pos+=1
                if pos<len(buf):
                    if not want_value:
                        if buf[pos]!=',':raise ValueError('Missing JSON array comma')
                        pos+=1;want_value=True;continue
                    try:value,end=decoder.raw_decode(buf,pos)
                    except json.JSONDecodeError:
                        if remaining==0:raise
                    else:
                        if remaining and not isinstance(value,(dict,list,str)) and (end==len(buf)or buf[end]not in ' \t\r\n,'):
                            pass # A scalar token may continue in the next chunk.
                        else:
                            pos=end;want_value=False;seen=True;yield value;continue
                if not remaining:
                    if want_value and seen:raise ValueError('Trailing JSON array comma')
                    return
                if len(buf)-pos>MAX_VALUE:raise ValueError('Receipt record exceeds 16 MiB')
                block=stream.read(min(1024*1024,remaining))
                if not block:raise ValueError('Truncated JSON array')
                remaining-=len(block);buf=buf[pos:]+utf8.decode(block,final=not remaining);pos=0
    def __len__(self):return sum(1 for _ in self)


class Receipt(Mapping):
    def __init__(self,path):
        self.path=Path(path);self.fields={}
        with self.path.open('rb')as stream,mmap.mmap(stream.fileno(),0,access=mmap.ACCESS_READ)as view:
            pos=3 if view[:3]==b'\xef\xbb\xbf'else 0
            pos=_space(view,pos)
            if view[pos]!=123:raise ValueError('Receipt must be a JSON object')
            pos=_space(view,pos+1)
            while view[pos]!=125:
                key=STRING.match(view,pos)
                if not key:raise ValueError('Expected root receipt key')
                name=json.loads(key.group());pos=_space(view,key.end())
                if name in self.fields:raise ValueError('Duplicate root receipt key')
                if view[pos]!=58:raise ValueError('Expected JSON colon')
                start=_space(view,pos+1);end=_end(view,start)
                self.fields[name]=(start,end,view[start]==91)
                pos=_space(view,end)
                if view[pos]==125:break
                if view[pos]!=44:raise ValueError('Expected JSON root comma')
                pos=_space(view,pos+1)
                if view[pos]==125:raise ValueError('Trailing JSON root comma')
            if _space(view,pos+1)!=len(view):raise ValueError('Trailing receipt data')
    def __getitem__(self,key):
        start,end,array=self.fields[key]
        if array:return ArrayView(self.path,start,end)
        if end-start>MAX_VALUE:raise ValueError('Receipt metadata field exceeds 16 MiB')
        with self.path.open('rb')as stream:
            stream.seek(start);return json.loads(stream.read(end-start))
    def __iter__(self):return iter(self.fields)
    def __len__(self):return len(self.fields)


def compact_quote(snapshot):
    """Retain only inputs needed by the analyzer, with exact 25 bp depth sums."""
    bid,ask=snapshot['book']['bids'],snapshot['book']['asks']
    if not bid or not ask:
        return dict(time_ms=snapshot['time_ms'],book=dict(bids=bid[:1],asks=ask[:1]))
    mid=(bid[0]['price_tick']+ask[0]['price_tick'])/2
    bids=[dict(price_tick=bid[0]['price_tick'],qty=sum(x['qty']for x in bid if x['price_tick']>=mid*(1-.0025)))]
    asks=[dict(price_tick=ask[0]['price_tick'],qty=sum(x['qty']for x in ask if x['price_tick']<=mid*(1+.0025)))]
    return dict(time_ms=snapshot['time_ms'],book=dict(bids=bids,asks=asks))
