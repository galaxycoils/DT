"""Extract static form descriptors as inert HTML data; keep runtime code small."""
import ast
import json
from pathlib import Path
root=Path('crates/gm-openbot/src')
source=(root/'companion_ui.source.js').read_text()
schema={}
edits=[]

def end_arg(start):
    depth=0
    quote=None
    escape=False
    for i in range(start,len(source)):
        c=source[i]
        if quote:
            if escape: escape=False
            elif c=='\\': escape=True
            elif c==quote: quote=None
        elif c in "'\"`": quote=c
        elif c in '([{': depth+=1
        elif c in ')]}':
            if depth==0: return i
            depth-=1
        elif c==',' and depth==0: return i
    raise ValueError('Unterminated argument')

import re
for match in re.finditer(r'\bform\(',source):
    start=match.end()
    for _ in range(2): start=end_arg(start)+1
    end=end_arg(start)
    value=source[start:end].strip()
    if not value.startswith('['): continue
    try: descriptor=ast.literal_eval(value)
    except (ValueError,SyntaxError): continue
    name='f'+str(len(schema))
    schema[name]=descriptor
    edits.append((start,end,'schemas.'+name))
for start,end,value in reversed(edits): source=source[:start]+value+source[end:]
# Static copy is inert document data, not executable source or untrusted markup.
copy=[]
strings=[]
for match in re.finditer(r"'(?:[^'\\]|\\.)*'|\"(?:[^\"\\]|\\.)*\"",source):
    try: value=ast.literal_eval(match.group())
    except (ValueError,SyntaxError): continue
    if not isinstance(value,str) or len(value)<5 or value.startswith(('/api/','/ag-ui/')): continue
    if source[match.end():].lstrip().startswith(':'): continue
    if value not in strings: strings.append(value)
    copy.append((match.start(),match.end(),'texts['+str(strings.index(value))+']'))
for start,end,value in reversed(copy): source=source[:start]+value+source[end:]
schema['s']=strings
source="const schemas=JSON.parse(document.getElementById('ui-schema').textContent),texts=schemas.s;\n"+source
Path('/tmp/grok-companion-build.js').write_text(source)
html=(root/'companion_ui.source.html').read_text()
data=json.dumps(schema,separators=(',',':'),ensure_ascii=False).replace('<','\\u003c')
html=html.replace('</body>',f'<script id="ui-schema" type="application/json">{data}</script></body>')
(root/'companion_ui.html').write_text(html)
print(f'Extracted {len(schema)} form descriptors ({len(data)} bytes)')
