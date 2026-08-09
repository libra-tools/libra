#!/bin/sh
# tests/compat-ledger/t4/PROJECT_DRAFT.sh <frozen-draft> <drop-fn-list> <out>
#   或 tests/compat-ledger/t4/PROJECT_DRAFT.sh --digest <frozen-draft> <out>
# 模式一：从冻结草稿中删除 drop 清单所列的 `t4_port_*` 函数（连同其前置 `///` 文档块与属性），
#         其余逐字保留。
# 模式二（`--digest`，R47）：用**同一个词法扫描器**逐函数输出 `<fn>\t<规范化正文 sha256>`——
#         断言锁的生成与校验都必须走这里，杜绝「两处各写一份边界扫描」的实现漂移。
set -eu
if [ "${1:-}" = "--digest" ]; then
  [ $# -eq 3 ] || { echo "usage: PROJECT_DRAFT.sh --digest <draft> <out>" >&2; exit 2; }
  set -- "$2" /dev/null "$3" --digest
else
  [ $# -eq 3 ] || { echo "usage: PROJECT_DRAFT.sh <draft> <drop-list> <out>" >&2; exit 2; }
  set -- "$1" "$2" "$3" ""
fi
python3 - "$1" "$2" "$3" "$4" <<'PY'
import re, sys
src=open(sys.argv[1],encoding='utf-8').read()
drop={l.strip() for l in open(sys.argv[2],encoding='utf-8') if l.strip()}

def match_brace(s, i):
    """从 s[i]=='{' 起返回匹配闭括号下标；**跳过**行/块注释、字符/普通/raw 字符串，
    避免把文本里的花括号当成边界（R46 P1）。找不到匹配即抛错。"""
    assert s[i]=='{'
    depth=0; j=i; n=len(s)
    while j < n:
        c=s[j]
        if c=='/' and j+1<n and s[j+1]=='/':
            j=s.find('\n', j);  j = n if j<0 else j+1;  continue
        if c=='/' and j+1<n and s[j+1]=='*':
            # Rust 块注释可嵌套（R47/R48）：按深度扫描到归零
            d2=1; j+=2
            while j<n and d2>0:
                if s.startswith('/*', j): d2+=1; j+=2; continue
                if s.startswith('*/', j): d2-=1; j+=2; continue
                j+=1
            if d2!=0: raise SystemExit("FAIL: unterminated block comment")
            continue
        if c=='r' and j+1<n and s[j+1] in '"#':
            k=j+1; h=0
            while k<n and s[k]=='#': h+=1; k+=1
            if k<n and s[k]=='"':
                end='"'+'#'*h; k=s.find(end, k+1)
                if k<0: raise SystemExit("FAIL: unterminated raw string")
                j=k+len(end); continue
        if c=='"':
            j+=1
            while j<n:
                if s[j]=='\\': j+=2; continue
                if s[j]=='"': break
                j+=1
            j+=1; continue
        if c=="'":
            # 字符字面量或生命周期：仅当形如 '\?x' 才按字面量跳过
            m=re.match(r"'(\\.|[^\\'])'", s[j:])
            if m: j+=m.end(); continue
            j+=1; continue
        if c=='{': depth+=1
        elif c=='}':
            depth-=1
            if depth==0: return j
        j+=1
    raise SystemExit("FAIL: unmatched opening brace")

def start_of_item(s, fn_start):
    """把删除区间上沿扩展到该函数**前置的连续 `///` 文档块与属性行**（R46 P0-2：
    只删函数体会留下归属错误或不可编译的孤立文档注释）。"""
    lines=s[:fn_start].split('\n')
    k=len(lines)-1
    # lines[-1] 是 fn 所在行的前缀（缩进）
    i=k-1
    while i>=0:
        t=lines[i].strip()
        if t.startswith('///') or t.startswith('#[') or t.startswith('#!['):
            i-=1; continue
        break
    return len('\n'.join(lines[:i+1])) + (1 if i>=0 else 0)

def scan_items(s):
    """单遍词法扫描（R49 P1）：只在 **normal 状态**（不在注释/字符/普通或 raw 字符串内）识别
    `fn <t4_port_*>` token，并由同一状态机定位其函数体开括号——避免注释、raw string 或守卫
    内联反例中的 `fn t4_port_x(` 被误认为真实函数，也避免从文档/属性中的 `{` 起算摘要。"""
    out=[]; i=0; n=len(s)
    while i < n:
        c=s[i]
        if c=='/' and i+1<n and s[i+1]=='/':
            k=s.find('\n', i); i = n if k<0 else k+1; continue
        if c=='/' and i+1<n and s[i+1]=='*':
            d=1; i+=2
            while i<n and d>0:
                if s.startswith('/*', i): d+=1; i+=2; continue
                if s.startswith('*/', i): d-=1; i+=2; continue
                i+=1
            if d!=0: raise SystemExit("FAIL: unterminated block comment")
            continue
        if c=='r' and i+1<n and s[i+1] in '"#':
            k=i+1; h=0
            while k<n and s[k]=='#': h+=1; k+=1
            if k<n and s[k]=='"':
                end='"'+'#'*h; k=s.find(end, k+1)
                if k<0: raise SystemExit("FAIL: unterminated raw string")
                i=k+len(end); continue
        if c=='"':
            i+=1
            while i<n:
                if s[i]=='\\': i+=2; continue
                if s[i]=='"': break
                i+=1
            i+=1; continue
        if c=="'":
            m2=re.match(r"'(\\.|[^\\'])'", s[i:])
            i += m2.end() if m2 else 1; continue
        m3=re.match(r'fn\s+(t4_port_[A-Za-z0-9_]+)\s*\(', s[i:])
        if m3 and (i==0 or not (s[i-1].isalnum() or s[i-1]=='_')):
            name=m3.group(1)
            k=i+m3.end()-1
            # R62 P1：参数列表与返回类型/where 段同样要**跳过注释与字面量**——裸扫时
            # 合法的 `fn t4_port_x(/* ) { */) {}` 会把注释里的 `)`/`{` 当成边界。
            # `skip_noncode` 复用同一套状态机：命中注释/字符串/字符/raw-string 就整体跳过。
            def skip_noncode(t, j):
                m=len(t)
                if t.startswith('//', j):
                    e=t.find('\n', j); return m if e<0 else e
                if t.startswith('/*', j):
                    d2=1; j+=2
                    while j<m and d2>0:
                        if t.startswith('/*', j): d2+=1; j+=2; continue
                        if t.startswith('*/', j): d2-=1; j+=2; continue
                        j+=1
                    if d2!=0: raise SystemExit("FAIL: unterminated block comment in signature")
                    return j
                if t[j]=='r' and j+1<m and t[j+1] in '"#':
                    q=j+1; h2=0
                    while q<m and t[q]=='#': h2+=1; q+=1
                    if q<m and t[q]=='"':
                        e2='"'+'#'*h2; q=t.find(e2, q+1)
                        if q<0: raise SystemExit("FAIL: unterminated raw string in signature")
                        return q+len(e2)
                if t[j]=='"':
                    j+=1
                    while j<m:
                        if t[j]=='\\': j+=2; continue
                        if t[j]=='"': return j+1
                        j+=1
                    raise SystemExit("FAIL: unterminated string in signature")
                if t[j]=="'":
                    mm=re.match(r"'(\\.|[^\\'])'", t[j:])
                    return j+(mm.end() if mm else 1)
                return None
            depth_p=1; k+=1
            while k<n and depth_p>0:            # 跳过参数列表（注释/字面量整体跳过）
                nk=skip_noncode(s, k)
                if nk is not None: k=nk; continue
                if s[k]=='(': depth_p+=1
                elif s[k]==')': depth_p-=1
                k+=1
            while k<n and s[k]!='{':             # 返回类型/where 之后的函数体开括号
                nk=skip_noncode(s, k)
                k = nk if nk is not None else k+1
            if k>=n: raise SystemExit("FAIL: no body for "+name)
            body_start=k
            body_end=match_brace(s, k)
            out.append((name, i, body_start, body_end+1))
            i=body_end+1; continue
        i+=1
    return out

def normalize_body(text):
    """折叠字面量**之外**的空白；字符串/字符/raw-string 内容逐字节保留（R60 P1）。"""
    out=[]; i=0; n=len(text); pending_ws=False
    while i < n:
        c=text[i]
        # raw string: r"..." / r#"..."# / r##"..."## …
        if c=='r' and i+1 < n and (text[i+1]=='"' or text[i+1]=='#'):
            j=i+1; hashes=0
            while j < n and text[j]=='#': hashes+=1; j+=1
            if j < n and text[j]=='"':
                close='"'+'#'*hashes
                k=text.find(close, j+1)
                k=n if k==-1 else k+len(close)
                if pending_ws: out.append(' '); pending_ws=False
                out.append(text[i:k]); i=k; continue
        if c=='"' or c=="'":
            q=c; j=i+1
            while j < n:
                if text[j]=='\\': j+=2; continue
                if text[j]==q: j+=1; break
                j+=1
            if pending_ws: out.append(' '); pending_ws=False
            out.append(text[i:j]); i=j; continue
        if c.isspace(): pending_ws=True; i+=1; continue
        if pending_ws: out.append(' '); pending_ws=False
        out.append(c); i+=1
    return ''.join(out).strip()

spans=[]
for name, fn_at, body_start, end in scan_items(src):
    spans.append((name, start_of_item(src, fn_at), end, body_start))
seen={n for n,_,_,_ in spans}
missing=sorted(drop-seen)
if missing:
    print("FAIL: drop list names not present in draft:", missing); sys.exit(1)
if len(sys.argv)>4 and sys.argv[4]=='--digest':
    import hashlib
    rows=[]
    for name,a,b,body_start in spans:
        body=src[body_start:b]                  # 函数体（含大括号），起点由词法扫描给出
        # R60 P1：**不得**对整个函数体做 `\s+ -> ' '` 归一化——那会把**字符串/字符字面量
        # 内部**的空白一并压平，使 `assert_eq!(out, "a  b")` 与 `assert_eq!(out, "a b")`
        # 得到同一摘要，从而绕过断言完整性门。改为「字面量内部逐字节保留、字面量之外才折叠
        # 空白」的词法归一化：复用同一个扫描器的字符串/字符/raw-string 状态机。
        norm = normalize_body(body)
        rows.append((name, hashlib.sha256(norm.encode()).hexdigest()))
    with open(sys.argv[3],'w') as f:
        for n,h in sorted(rows): f.write(f"{n}\t{h}\n")
    sys.exit(0)
out=[]; pos=0
for name,a,b,_bs in spans:
    if a < pos: raise SystemExit("FAIL: overlapping item spans")
    out.append(src[pos:a])
    if name not in drop: out.append(src[a:b])
    pos=b
out.append(src[pos:])
open(sys.argv[3],'w').write(''.join(out))
PY
