"""Trusted file conversions, run in the converter sandbox."""
import datetime
import json
import sys

operation = sys.argv[1]

if operation == "read_numbers":
    from numbers_parser import Document
    document = Document("input.numbers")
    tabs = []
    for sheet in document.sheets:
        for table in sheet.tables:
            rows = []
            for row in table.rows():
                cells = []
                for cell in row:
                    value = cell.value
                    if isinstance(value, (datetime.datetime, datetime.date)):
                        value = value.isoformat()
                    elif isinstance(value, datetime.timedelta):
                        value = value.total_seconds() / 86400
                    formula = getattr(cell, "formula", None)
                    cells.append({"v": value, "f": "=" + formula.lstrip("=")} if isinstance(formula, str) and formula else value)
                rows.append(cells)
            name = sheet.name if len(sheet.tables) == 1 else sheet.name + " " + table.name
            tabs.append({"name": name, "rows": rows})
    with open("output.json", "w") as stream:
        json.dump({"tabs": tabs}, stream, default=str)
else:
    with open("input.json") as stream:
        tabs = json.load(stream)["tabs"]
    if operation == "write_xls":
        import xlwt
        book = xlwt.Workbook()
        style_cache = {}
        palette = {}
        def color(value):
            if value not in palette:
                index = 8 + len(palette)
                if index >= 64:
                    return 0x7fff
                rgb = tuple(int(value[i:i+2], 16) for i in (1,3,5))
                book.set_colour_RGB(index, *rgb)
                palette[value] = index
            return palette[value]
        def style(spec):
            key = json.dumps(spec, sort_keys=True)
            if key not in style_cache:
                s = xlwt.XFStyle()
                s.font.bold = spec.get("bold", False)
                s.font.italic = spec.get("italic", False)
                s.font.struck_out = spec.get("strike", False)
                s.font.underline = int(spec.get("underline", False))
                s.font.height = spec.get("font_size", 11) * 20
                s.font.name = spec.get("font", "Arial")
                if spec.get("color"): s.font.colour_index = color(spec["color"])
                if spec.get("fill"):
                    s.pattern.pattern = xlwt.Pattern.SOLID_PATTERN
                    s.pattern.pattern_fore_colour = color(spec["fill"])
                s.num_format_str = spec.get("num_fmt", "General")
                s.alignment.wrap = spec.get("wrap", False)
                s.alignment.horz = {"left":1,"center":2,"right":3}.get(spec.get("h_align"),0)
                style_cache[key] = s
            return style_cache[key]
        used = set()
        for tab in tabs:
            name = tab["name"][:31]
            suffix = 1
            while name.lower() in used:
                suffix += 1
                name = tab["name"][:25] + str(suffix)
            used.add(name.lower())
            sheet = book.add_sheet(name)
            for r, row in enumerate(tab["rows"]):
                for c, cell in enumerate(row):
                    value = cell["v"]
                    if cell.get("f"):
                        # A format limitation is an explicit conversion failure, never a silently lost formula.
                        value = xlwt.Formula(cell["f"].lstrip("="))
                    sheet.write(r, c, value, style(cell["style"]))
        book.save("output.xls")
    elif operation == "write_numbers":
        from numbers_parser import Document, Style, RGB
        document = None
        for tab in tabs:
            rows = tab["rows"]
            ncols = max((len(row) for row in rows), default=1)
            if document is None:
                document = Document(sheet_name=tab["name"], table_name=tab["name"], num_rows=max(1,len(rows)), num_cols=max(1,ncols), num_header_cols=0, num_header_rows=0)
            else:
                document.add_sheet(sheet_name=tab["name"], table_name=tab["name"], num_rows=max(1,len(rows)), num_cols=max(1,ncols))
            table = document.sheets[-1].tables[0]
            for r, row in enumerate(rows):
                for c, cell in enumerate(row):
                    value = cell["v"]
                    if cell.get("f"):
                        raise ValueError("Numbers formula writing is unavailable in numbers-parser; download XLSX to retain formulas.")
                    spec = cell["style"]
                    s = Style(bold=spec.get("bold",False), italic=spec.get("italic",False), underline=spec.get("underline",False), strikethrough=spec.get("strike",False), font_size=spec.get("font_size",11), font_name=spec.get("font","Helvetica Neue"), text_wrap=spec.get("wrap",False))
                    def rgb(text): return RGB(*(int(text[i:i+2],16) for i in (1,3,5)))
                    if spec.get("color"): s.font_color = rgb(spec["color"])
                    if spec.get("fill"): s.bg_color = rgb(spec["fill"])
                    if value is not None: table.write(r,c,value,style=s)
        document.save("output.numbers")
