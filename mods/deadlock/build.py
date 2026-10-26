#!/usr/bin/env python3
import argparse
import hashlib
import os
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
DEFAULT_OUT = REPO / "apps" / "afterglow" / "assets" / "afterglow_events_dir.vpk"
DEFAULT_GAMES = [
    os.environ.get("DEADLOCK_DIR", ""),
    r"F:\SteamLibrary\steamapps\common\Deadlock",
    "/mnt/f/SteamLibrary/steamapps/common/Deadlock",
    r"C:\Program Files (x86)\Steam\steamapps\common\Deadlock",
]

HOOK = "panorama/layout/hud_data_feed.vxml_c"
SCRIPT = "panorama/scripts/afterglow/events.vjs_c"
SCRIPT_REF = "panorama/scripts/afterglow/events.vjs"


def vpk_read(dir_path):
    data = Path(dir_path).read_bytes()
    sig, version, tree_size = struct.unpack_from("<III", data, 0)
    if sig != 0x55AA1234 or version not in (1, 2):
        raise ValueError(f"{dir_path}: not a VPK")
    header = 12 if version == 1 else 28
    entries = {}
    p = header

    def cstr():
        nonlocal p
        end = data.index(b"\0", p)
        s = data[p:end].decode("utf-8")
        p = end + 1
        return s

    while True:
        ext = cstr()
        if not ext:
            break
        while True:
            folder = cstr()
            if not folder:
                break
            while True:
                name = cstr()
                if not name:
                    break
                crc, preload, archive, offset, length, term = struct.unpack_from("<IHHIIH", data, p)
                p += 18
                pre = data[p : p + preload]
                p += preload
                path = (f"{folder}/" if folder != " " else "") + name + (f".{ext}" if ext != " " else "")
                entries[path] = (crc, pre, archive, offset, length)
    base = Path(dir_path)
    stem = base.name[: -len("_dir.vpk")]

    def get(path):
        crc, pre, archive, offset, length = entries[path]
        if archive == 0x7FFF:
            body = data[header + tree_size + offset : header + tree_size + offset + length]
        else:
            with open(base.with_name(f"{stem}_{archive:03}.vpk"), "rb") as f:
                f.seek(offset)
                body = f.read(length)
        out = pre + body
        if zlib.crc32(out) != crc:
            raise ValueError(f"{path}: CRC mismatch")
        return out

    return entries, get


def vpk_write(files):
    tree = bytearray()
    blob = bytearray()
    by_ext = {}
    for path in sorted(files):
        folder, _, leaf = path.rpartition("/")
        name, dot, ext = leaf.rpartition(".")
        if not dot:
            name, ext = leaf, " "
        by_ext.setdefault(ext, {}).setdefault(folder or " ", []).append((name, files[path]))
    for ext in sorted(by_ext):
        tree += ext.encode() + b"\0"
        for folder in sorted(by_ext[ext]):
            tree += folder.encode() + b"\0"
            for name, body in by_ext[ext][folder]:
                tree += name.encode() + b"\0"
                tree += struct.pack("<IHHIIH", zlib.crc32(body), 0, 0x7FFF, len(blob), len(body), 0xFFFF)
                blob += body
            tree += b"\0"
        tree += b"\0"
    tree += b"\0"
    archive_md5 = b""
    header = struct.pack("<IIIIIII", 0x55AA1234, 2, len(tree), len(blob), len(archive_md5), 48, 0)
    out = bytearray(header + tree + blob + archive_md5)
    out += hashlib.md5(tree).digest() + hashlib.md5(archive_md5).digest()
    out += hashlib.md5(out).digest()
    return bytes(out)


RES_HEADER_VERSION = 12


def res_read(data):
    size, header_version, version, block_offset, count = struct.unpack_from("<IHHII", data, 0)
    if header_version != RES_HEADER_VERSION or size != len(data):
        raise ValueError("not a resource this builder knows")
    blocks = []
    p = 8 + block_offset
    for i in range(count):
        at = p + i * 12
        fourcc = data[at : at + 4].decode("ascii")
        rel, length = struct.unpack_from("<II", data, at + 4)
        start = at + 4 + rel
        blocks.append((fourcc, data[start : start + length]))
    return version, blocks


def res_write(version, blocks, align=16):
    table_end = 16 + 12 * len(blocks)
    at = table_end
    starts = []
    for _, body in blocks:
        at = (at + align - 1) // align * align if len(blocks) > 1 else at
        starts.append(at)
        at += len(body)
    out = bytearray(struct.pack("<IHHII", at, RES_HEADER_VERSION, version, 8, len(blocks)))
    for i, (fourcc, body) in enumerate(blocks):
        field = 16 + i * 12 + 4
        out += fourcc.encode("ascii") + struct.pack("<II", starts[i] - field, len(body))
    for start, (_, body) in zip(starts, blocks):
        out += b"\0" * (start - len(out))
        out += body
    return bytes(out)


def script_resource(js):
    return res_write(4, [("DATA", js)])


KV3_MAGIC = b"VKV\x03"
KV3_ENCODING_BINARY_UNCOMPRESSED = bytes.fromhex("0005861bd8f7c140ad8275a48267e714")

NULL, BOOL, INT64, UINT64, DOUBLE, STRING, BLOB, ARRAY, OBJECT, TYPED = 1, 2, 3, 4, 5, 6, 7, 8, 9, 10
INT32, UINT32, TRUE, FALSE, I64_0, I64_1, D_0, D_1 = 11, 12, 13, 14, 15, 16, 17, 18
SCALARS = {BOOL: "<?", INT64: "<q", UINT64: "<Q", DOUBLE: "<d", INT32: "<i", UINT32: "<I"}
EMPTY = (NULL, TRUE, FALSE, I64_0, I64_1, D_0, D_1)


class Node:

    __slots__ = ("type", "flag", "value")

    def __init__(self, type_, value=None, flag=None):
        self.type, self.value, self.flag = type_, value, flag

    def get(self, key):
        return next((v for k, v in self.value if k == key), None)


def kv3_read(data):
    if data[:4] == b"\x053VK":
        return kv3_read_v5(data)
    return kv3_read_legacy(data)


def kv3_read_legacy(data):
    if data[:4] != KV3_MAGIC or data[4:20] != KV3_ENCODING_BINARY_UNCOMPRESSED:
        raise ValueError("not uncompressed legacy binary KV3")
    fmt = data[20:36]
    p = 36
    (count,) = struct.unpack_from("<I", data, p)
    p += 4
    strings = []
    for _ in range(count):
        end = data.index(b"\0", p)
        strings.append(data[p:end].decode("utf-8"))
        p = end + 1

    def typ():
        nonlocal p
        t = data[p]
        p += 1
        flag = None
        if t & 0x80:
            flag = data[p]
            p += 1
        return t & 0x7F, flag

    def string(i):
        return None if i == -1 else strings[i]

    def value(t, flag):
        nonlocal p
        if t in EMPTY:
            return Node(t, None, flag)
        if t in SCALARS:
            f = SCALARS[t]
            (v,) = struct.unpack_from(f, data, p)
            p += struct.calcsize(f)
            return Node(t, v, flag)
        if t == STRING:
            (i,) = struct.unpack_from("<i", data, p)
            p += 4
            return Node(t, string(i), flag)
        if t == BLOB:
            (n,) = struct.unpack_from("<i", data, p)
            p += 4
            v = data[p : p + n]
            p += n
            return Node(t, v, flag)
        if t == ARRAY:
            (n,) = struct.unpack_from("<i", data, p)
            p += 4
            return Node(t, [value(*typ()) for _ in range(n)], flag)
        if t == TYPED:
            (n,) = struct.unpack_from("<i", data, p)
            p += 4
            st, sf = typ()
            return Node(t, (st, sf, [value(st, sf) for _ in range(n)]), flag)
        if t == OBJECT:
            (n,) = struct.unpack_from("<i", data, p)
            p += 4
            members = []
            for _ in range(n):
                (k,) = struct.unpack_from("<i", data, p)
                p += 4
                members.append((string(k), value(*typ())))
            return Node(t, members, flag)
        raise ValueError(f"KV3 type {t} at {p}")

    root = value(*typ())
    if struct.unpack_from("<I", data, p)[0] != 0xFFFFFFFF:
        raise ValueError("KV3 trailer missing")
    if p + 4 != len(data):
        raise ValueError("bytes after the KV3 trailer")
    return fmt, root


FLOAT, INT16, UINT16, INT8, UINT8, TYPED_BYTE, TYPED_AUX, U32_0, U32_1 = 19, 20, 21, 22, 23, 24, 25, 26, 27
LANES = {
    BOOL: (1, "<B", BOOL), INT8: (1, "<b", INT32), UINT8: (1, "<B", UINT32),
    INT16: (2, "<h", INT32), UINT16: (2, "<H", UINT32),
    INT32: (4, "<i", INT32), UINT32: (4, "<I", UINT32), FLOAT: (4, "<f", DOUBLE),
    INT64: (8, "<q", INT64), UINT64: (8, "<Q", UINT64), DOUBLE: (8, "<d", DOUBLE),
}
LEGACY_FLAG_BITS = {1: 1, 2: 2, 3: 8, 4: 16, 5: 32}


def lz4_block(src, size):
    dst = bytearray()
    i = 0
    while i < len(src):
        token = src[i]
        i += 1
        n = token >> 4
        if n == 15:
            while True:
                b = src[i]
                i += 1
                n += b
                if b != 255:
                    break
        dst += src[i : i + n]
        i += n
        if i >= len(src):
            break
        back = src[i] | src[i + 1] << 8
        i += 2
        n = token & 15
        if n == 15:
            while True:
                b = src[i]
                i += 1
                n += b
                if b != 255:
                    break
        n += 4
        start = len(dst) - back
        if back >= n:
            dst += dst[start : start + n]
        else:
            for k in range(n):
                dst.append(dst[start + k])
    if len(dst) != size:
        raise ValueError("LZ4 block size mismatch")
    return bytes(dst)


def kv3_read_v5(data):
    fmt = data[4:20]
    (method, dictionary, _frame, b1, b4, b8, ntypes, _objs, _arrs, _total, _ctotal, blocks, _blob_bytes, b2, _bcs,
     u1, c1, u2, c2, b1_2, b2_2, b4_2, b8_2, _nodes, objs_2, _arrays, _elems) = struct.unpack_from("<IHHiiiiHHiiiiiiiiiiiiiiiiii", data, 20)
    p = 20 + struct.calcsize("<IHHiiiiHHiiiiiiiiiiiiiiiiii")
    if dictionary != 0 or blocks != 0 or method not in (0, 1):
        raise ValueError("KV3 v5 with blobs, dictionaries or zstd: not needed for layouts so far")

    def buffer(raw_size, packed_size):
        nonlocal p
        chunk = data[p : p + packed_size]
        p += packed_size
        return chunk if method == 0 else lz4_block(chunk, raw_size)

    buf1, buf2 = buffer(u1, c1), buffer(u2, c2)

    def lanes(buf, at, counts):
        out = {}
        for width, count in zip((1, 2, 4, 8), counts):
            if count:
                at = (at + width - 1) // width * width
                out[width] = [buf, at]
                at += width * count
            else:
                out[width] = [buf, at]
        return out, at

    aux, end1 = lanes(buf1, 0, (b1, b2, b4, b8))
    main, end2 = lanes(buf2, objs_2 * 4, (b1_2, b2_2, b4_2, b8_2))
    types = [buf2, end2]
    lengths = [buf2, 0]

    def take(lane, fmt_):
        buf, at = lane
        (v,) = struct.unpack_from(fmt_, buf, at)
        lane[1] = at + struct.calcsize(fmt_)
        return v

    count = take(aux[4], "<i")
    strings = []
    sb, sp = aux[1]
    for _ in range(count):
        e = sb.index(b"\0", sp)
        strings.append(sb[sp:e].decode("utf-8"))
        sp = e + 1
    aux[1][1] = sp

    def typ():
        t = take(types, "<B")
        flag = None
        if t & 0x80:
            f = take(types, "<B")
            if f not in LEGACY_FLAG_BITS:
                raise ValueError(f"KV3 flag {f}")
            flag = LEGACY_FLAG_BITS[f]
        if t & 0x40:
            take(types, "<B")
        return t & 0x3F, flag

    def string(i):
        return None if i == -1 else strings[i]

    def value(t, flag, lane):
        if t in EMPTY:
            return Node(t, None, flag)
        if t in (U32_0, U32_1):
            return Node(UINT32, t - U32_0, flag)
        if t in LANES:
            width, f, legacy = LANES[t]
            v = take(lane[width], f)
            return Node(legacy, bool(v) if t == BOOL else v, flag)
        if t == STRING:
            return Node(t, string(take(main[4], "<i")), flag)
        if t == ARRAY:
            n = take(main[4], "<i")
            return Node(t, [value(*typ(), main) for _ in range(n)], flag)
        if t in (TYPED, TYPED_BYTE, TYPED_AUX):
            n = take(main[4], "<i") if t == TYPED else take(main[1], "<B")
            st, sf = typ()
            el = aux if t == TYPED_AUX else main
            items = [value(st, sf, el) for _ in range(n)]
            legacy = items[0].type if items else st
            return Node(TYPED, (legacy, sf, items), flag)
        if t == OBJECT:
            n = take(lengths, "<i")
            members = []
            for _ in range(n):
                vt, vf = typ()
                k = string(take(main[4], "<i"))
                members.append((k, value(vt, vf, main)))
            return Node(t, members, flag)
        raise ValueError(f"KV3 v5 type {t}")

    root = value(*typ(), main)
    if take(types, "<I") != 0xFFEEDD00:
        raise ValueError("KV3 v5 trailer missing")
    return fmt, root


def kv3_write(fmt, root):
    strings, index = [], {}

    def sid(s):
        if s is None:
            return -1
        if s not in index:
            index[s] = len(strings)
            strings.append(s)
        return index[s]

    body = bytearray()

    def typ(t, flag):
        if flag is None:
            body.append(t)
        else:
            body.append(t | 0x80)
            body.append(flag)

    def value(n):
        t = n.type
        if t in EMPTY:
            return
        if t in SCALARS:
            body.extend(struct.pack(SCALARS[t], n.value))
        elif t == STRING:
            body.extend(struct.pack("<i", sid(n.value)))
        elif t == BLOB:
            body.extend(struct.pack("<i", len(n.value)) + n.value)
        elif t == ARRAY:
            body.extend(struct.pack("<i", len(n.value)))
            for v in n.value:
                typ(v.type, v.flag)
                value(v)
        elif t == TYPED:
            st, sf, items = n.value
            body.extend(struct.pack("<i", len(items)))
            typ(st, sf)
            for v in items:
                value(v)
        elif t == OBJECT:
            body.extend(struct.pack("<i", len(n.value)))
            for k, v in n.value:
                body.extend(struct.pack("<i", sid(k)))
                typ(v.type, v.flag)
                value(v)
        else:
            raise ValueError(f"KV3 type {t}")

    typ(root.type, root.flag)
    value(root)
    table = struct.pack("<I", len(strings)) + b"".join(s.encode("utf-8") + b"\0" for s in strings)
    return KV3_MAGIC + KV3_ENCODING_BINARY_UNCOMPRESSED + fmt + table + bytes(body) + b"\xff\xff\xff\xff"


def ast_node(etype, child=None, name=None):
    members = [("eType", Node(STRING, etype))]
    if name is not None:
        members.append(("name", Node(STRING, name)))
    if child is not None:
        members.append(("child", child))
    return Node(OBJECT, members)


def children_of(node):
    keys = [k for k, _ in node.value]
    if "vecChildren" in keys:
        kids = node.value[keys.index("vecChildren")][1]
        return kids.value[2] if kids.type == TYPED else kids.value
    kids = [node.value.pop(keys.index("child"))[1]] if "child" in keys else []
    node.value.append(("vecChildren", Node(TYPED, (OBJECT, None, kids))))
    return kids


def etype(node):
    return node.get("eType").value


def add_script(layout, ref):
    version, blocks = res_read(layout)
    names = [b[0] for b in blocks]
    if "LaCo" not in names:
        raise ValueError("the layout has no LaCo block: the game's layout format changed")
    i = names.index("LaCo")
    fmt, root = kv3_read(blocks[i][1])
    ast_root = root.get("m_AST").get("m_pRoot")
    if etype(ast_root) != "ROOT":
        raise ValueError("unexpected layout tree")
    children = children_of(ast_root)
    include = ast_node("INCLUDE", ast_node("REFERENCE_COMPILED", name=ref))
    scripts = next((c for c in children if etype(c) == "SCRIPTS"), None)
    if scripts is None:
        styles = [n for n, c in enumerate(children) if etype(c) == "STYLES"]
        children.insert(styles[0] + 1 if styles else 0, ast_node("SCRIPTS", include))
    else:
        children_of(scripts).append(include)
    blocks[i] = ("LaCo", kv3_write(fmt, root))
    return res_write(version, blocks)


def plain(node):
    if node.type == OBJECT:
        return [(k, plain(v)) for k, v in node.value]
    if node.type == ARRAY:
        return [plain(v) for v in node.value]
    if node.type == TYPED:
        return ("typed", node.value[0], node.value[1], [plain(v) for v in node.value[2]])
    return (node.type, node.value, node.flag)


def find_game(arg):
    for d in [arg] + DEFAULT_GAMES if arg else DEFAULT_GAMES:
        if d and (Path(d) / "game" / "citadel" / "pak01_dir.vpk").is_file():
            return Path(d)
    sys.exit("Deadlock not found: pass --game <…\\steamapps\\common\\Deadlock> or set DEADLOCK_DIR")


def selftest(entries, get):
    layouts = [p for p in entries if p.endswith(".vxml_c")]
    for path in layouts:
        data = get(path)
        version, blocks = res_read(data)
        assert res_write(version, blocks) == data, f"{path}: resource layout differs"
        for fourcc, body in blocks:
            if fourcc == "LaCo":
                fmt, root = kv3_read(body)
                legacy = kv3_write(fmt, root)
                again = kv3_read(legacy)
                assert again[0] == fmt and plain(again[1]) == plain(root), f"{path}: layout tree differs"
                assert kv3_write(*again) == legacy, f"{path}: legacy KV3 differs"
    print(f"selftest: {len(layouts)} layouts round-trip")


def main():
    ap = argparse.ArgumentParser(description="builds afterglow's Deadlock addon VPK from src/events.js and the game's hud_data_feed layout")
    ap.add_argument("--game", help="the Deadlock install (…\\steamapps\\common\\Deadlock)")
    ap.add_argument("--out", default=str(DEFAULT_OUT))
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    game = find_game(a.game)
    entries, get = vpk_read(game / "game" / "citadel" / "pak01_dir.vpk")
    if a.selftest:
        selftest(entries, get)
        return
    js = (HERE / "src" / "events.js").read_bytes().replace(b"\r\n", b"\n")
    files = {
        HOOK: add_script(get(HOOK), SCRIPT_REF),
        SCRIPT: script_resource(js),
    }
    vpk = vpk_write(files)
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(vpk)
    print(f"{out}: {len(vpk)} bytes, md5 {hashlib.md5(vpk).hexdigest()}")


if __name__ == "__main__":
    main()
