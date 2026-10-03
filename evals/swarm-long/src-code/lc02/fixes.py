"""Reference fixes for lc02 (ledgerly): exact, unique text replacements."""

FIXES = {
    1: [
        ("ledgerly/rates.py",
         """        rate = to_decimal(rate)
        if rate <= 0:
            raise ValueError("rates must be positive")
""",
         """        currency = normalize_currency(currency)
        rate = to_decimal(rate)
        if rate <= 0:
            raise ValueError("rates must be positive")
"""),
        ("ledgerly/rates.py",
         """        if currency == self.base:
            return Decimal(1)
        dates = self._dates.get(currency)
        if not dates:
            raise RateNotFound("rate not found for %s" % currency)
        index = bisect.bisect_left(dates, day)
        if index == 0:
            raise RateNotFound("rate not found for %s" % currency)
""",
         """        currency = normalize_currency(currency)
        if currency == self.base:
            return Decimal(1)
        dates = self._dates.get(currency)
        index = bisect.bisect_right(dates, day) if dates else 0
        if index == 0:
            raise RateNotFound("no %s rate on or before %s" % (currency, day.isoformat()))
"""),
    ],
    2: [
        ("ledgerly/tax.py",
         """    digits = minor_units(currency)
    return Decimal(str(round(float(net) * float(rate), digits)))
""",
         """    return quantize(to_decimal(net) * to_decimal(rate), currency)
"""),
        ("ledgerly/tax.py",
         """    net = quantize(gross / (1 + rate), currency)
    tax = compute_tax(net, rate, currency)
    return net, tax
""",
         """    tax = quantize(gross * rate / (1 + rate), currency)
    net = gross - tax
    return net, tax
"""),
        ("ledgerly/invoicing.py",
         """        groups = {}
        for line in self.lines:
            code = table.get(line.tax_code)
            net, tax = groups.get(code.code, (ZERO, ZERO))
            line_net = line.net(self.currency)
            groups[code.code] = (net + line_net, tax + compute_tax(line_net, code.rate, self.currency))
        return [(code, groups[code][0], groups[code][1]) for code in sorted(groups)]
""",
         """        nets = {}
        for line in self.lines:
            code = table.get(line.tax_code)
            nets[code.code] = nets.get(code.code, ZERO) + line.net(self.currency)
        result = []
        for code in sorted(nets):
            rate = table.get(code).rate
            result.append((code, nets[code], compute_tax(nets[code], rate, self.currency)))
        return result
"""),
    ],
    3: [
        ("ledgerly/trial_balance.py",
         """    for code in sorted(totals):
        account = ledger.chart.get(code)
        debit, credit = totals[code]
        net = debit - credit
        rows.append(""",
         """    for account in ledger.chart:
        debit, credit = totals.get(account.code, (ZERO, ZERO))
        net = debit - credit
        if net == 0 and not include_zero:
            continue
        rows.append("""),
        ("ledgerly/formatting.py",
         """    return "{0:.{1}f}".format(value, minor_units(currency))""",
         """    return "{0:,.{1}f}".format(value, minor_units(currency))"""),
    ],
    4: [
        ("ledgerly/periods.py",
         """        return self.start <= day < self.end""",
         """        return self.start <= day <= self.end"""),
        ("ledgerly/periods.py",
         """    closing = JournalEntry(period.start, "Close %s" % period.name, reference=period.name)
    to_equity = ZERO
    for code in sorted(balances, key=code_key):
        balance = balances[code]
        if balance > 0:""",
         """    closing = JournalEntry(period.end, "Close %s" % period.name, reference=period.name)
    to_equity = ZERO
    for code in sorted(balances, key=code_key):
        balance = balances[code]
        if balance == 0:
            continue
        if balance > 0:"""),
        ("ledgerly/periods.py",
         """    ledger.post(closing)
    calendar.lock(period)
    return closing
""",
         """    if closing.lines:
        ledger.post(closing)
    else:
        closing = None
    calendar.lock(period)
    return closing
"""),
    ],
    5: [
        ("ledgerly/aging.py",
         """    return (as_of - item.invoice_date).days""",
         """    return (as_of - item.due_date).days"""),
        ("ledgerly/aging.py",
         """    if days < 30:
        return "1-30"
    if days < 60:
        return "31-60"
    if days < 90:
        return "61-90"
""",
         """    if days <= 30:
        return "1-30"
    if days <= 60:
        return "31-60"
    if days <= 90:
        return "61-90"
"""),
        ("ledgerly/aging.py",
         """    for payment in payments:
        pool[payment.customer] = pool.get(payment.customer, ZERO) + payment.amount
    for item in sorted(items, key=lambda i: i.invoice_date):
""",
         """    for payment in payments:
        if as_of is not None and payment.date > as_of:
            continue
        pool[payment.customer] = pool.get(payment.customer, ZERO) + payment.amount
    for item in sorted(items, key=lambda i: (i.due_date, i.number)):
"""),
    ],
    6: [
        ("ledgerly/journal.py",
         """        \"\"\"Return ``(debit, credit)`` in the ledger base currency.\"\"\"
        debit = sum_amounts(line.amount for line in self.lines if line.side == DEBIT)
        credit = sum_amounts(line.amount for line in self.lines if line.side == CREDIT)
""",
         """        \"\"\"Return ``(debit, credit)`` in the ledger base currency.\"\"\"
        debit = sum_amounts(line.debit for line in self.lines)
        credit = sum_amounts(line.credit for line in self.lines)
"""),
        ("ledgerly/fx.py",
         """    return Decimal(str(round(float(amount) * float(rate), minor_units(base))))""",
         """    return quantize(to_decimal(amount) * to_decimal(rate), base)"""),
        ("ledgerly/fx.py",
         """from .money import minor_units, quantize""",
         """from .money import quantize, to_decimal"""),
        ("ledgerly/fx.py",
         """    if rounding_account is None or abs(difference) >= limit:
        raise UnbalancedEntryError("entry unbalanced by %s %s after conversion" % (
            quantize(difference, base), base))""",
         """    if rounding_account is None or abs(difference) > limit:
        raise UnbalancedEntryError("entry unbalanced by %s %s after conversion" % (
            quantize(abs(difference), base), base))"""),
    ],
    7: [
        ("ledgerly/importer.py",
         """    cleaned = text.strip()
    if not cleaned:
        return ZERO
    try:""",
         """    cleaned = text.strip().replace(",", "")
    if not cleaned:
        return ZERO
    try:"""),
        ("ledgerly/importer.py",
         """    entries = {}
    order = []
    for line_no, row in enumerate(reader, start=1):
        ref = (row["entry"] or "").strip()
        try:
            if not ref:
                raise ValueError("missing entry reference")
            day, account, side, amount = _parse_row(row, chart)
        except ValueError as exc:
            raise ImportErrors(["line %d: %s" % (line_no, exc)])
""",
         """    entries = {}
    order = []
    errors = []
    failed = set()
    for row in reader:
        line_no = reader.line_num
        ref = (row["entry"] or "").strip()
        try:
            if not ref:
                raise ValueError("missing entry reference")
            day, account, side, amount = _parse_row(row, chart)
        except ValueError as exc:
            errors.append("line %d: %s" % (line_no, exc))
            failed.add(ref)
            continue
"""),
        ("ledgerly/importer.py",
         """    for ref in order:
        entry = entries[ref]
        if entry.currencies():
            continue
        debit, credit = entry.totals()
        if debit != credit:
            raise ImportErrors(["entry %s: unbalanced (debit %s, credit %s)" % (
                ref, quantize(debit), quantize(credit))])
    return [entries[ref] for ref in order]
""",
         """    for ref in order:
        entry = entries[ref]
        if ref in failed or entry.currencies():
            continue
        debit, credit = entry.totals()
        if debit != credit:
            errors.append("entry %s: unbalanced (debit %s, credit %s)" % (
                ref, quantize(debit), quantize(credit)))
    if errors:
        raise ImportErrors(errors)
    return [entries[ref] for ref in order]
"""),
    ],
}
