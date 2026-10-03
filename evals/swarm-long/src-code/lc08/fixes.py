"""Reference fixes for lc08 (splitbill): exact replacements per issue."""

FIXES = {
    1: [
        ("splitbill/money.py",
         """    base, rest = divmod(total, n)
    shares = [base] * n
    shares[-1] += rest
    return shares""",
         """    sign = -1 if total < 0 else 1
    base, rest = divmod(abs(total), n)
    shares = [base + 1 if i < rest else base for i in range(n)]
    return [sign * share for share in shares]"""),
    ],
    2: [
        ("splitbill/expenses.py",
         """    shares = collections.OrderedDict()
    for name, pct in percents.items():
        shares[_name(name)] = int(round(total * float(pct) / 100))
    return shares""",
         """    names = [_name(n) for n in percents]
    try:
        values = [Decimal(str(p).strip()) for p in percents.values()]
    except InvalidOperation:
        raise SplitError("bad percentage in %r" % (dict(percents),))
    if any(v < 0 for v in values):
        raise SplitError("negative percentage")
    if sum(values) != 100:
        raise SplitError("percentages add up to %s, not 100" % (sum(values),))
    try:
        shares = split_weighted(total, values)
    except MoneyError as exc:
        raise SplitError(str(exc))
    return collections.OrderedDict(zip(names, shares))"""),
    ],
    3: [
        ("splitbill/settle.py",
         """    debtors = sorted([-amount, name] for name, amount in balances.items() if amount < 0)
    creditors = sorted([amount, name] for name, amount in balances.items() if amount > 0)
    transfers = []
    i = j = 0
    while i < len(debtors) and j < len(creditors):
        amount = min(debtors[i][0], creditors[j][0])
        transfers.append((debtors[i][1], creditors[j][1], amount))
        debtors[i][0] -= amount
        creditors[j][0] -= amount
        if debtors[i][0] == 0:
            i += 1
        if creditors[j][0] == 0:
            j += 1
    return transfers""",
         """    if sum(balances.values()) != 0:
        raise SettleError("balances add up to %d, not 0" % sum(balances.values()))
    debts = {name: -amount for name, amount in balances.items() if amount < 0}
    credits = {name: amount for name, amount in balances.items() if amount > 0}
    transfers = []
    while debts and credits:
        debtor = min(debts, key=lambda name: (-debts[name], name))
        creditor = min(credits, key=lambda name: (-credits[name], name))
        amount = min(debts[debtor], credits[creditor])
        transfers.append((debtor, creditor, amount))
        debts[debtor] -= amount
        credits[creditor] -= amount
        if debts[debtor] == 0:
            del debts[debtor]
        if credits[creditor] == 0:
            del credits[creditor]
    return transfers"""),
    ],
    4: [
        ("splitbill/currency.py",
         """from decimal import Decimal, InvalidOperation
""",
         """from decimal import ROUND_HALF_UP, Decimal, InvalidOperation
"""),
        ("splitbill/currency.py",
         """    if src == dst:
        return cents
    amount = Decimal(cents) * rates[src] / rates[dst]
    return int(amount)""",
         """    src = normalize_code(src)
    dst = normalize_code(dst)
    for code in (src, dst):
        if code != BASE and code not in rates:
            raise CurrencyError("unknown currency %r" % (code,))
    if src == dst:
        return cents
    src_rate = Decimal(1) if src == BASE else rates[src]
    dst_rate = Decimal(1) if dst == BASE else rates[dst]
    amount = Decimal(cents) / src_rate * dst_rate
    return int(amount.quantize(Decimal(1), rounding=ROUND_HALF_UP))"""),
    ],
    5: [
        ("splitbill/importer.py",
         """            if amount <= 0:
                raise ImportFailed("line %d: amount must be positive" % line_no)""",
         """            if amount == 0:
                raise ImportFailed("line %d: amount must not be zero" % line_no)"""),
        ("splitbill/balances.py",
         """        if expense.amount <= 0:
            raise BalanceError("%s: amount must be positive" % (expense.description,))""",
         """        if expense.amount == 0:
            raise BalanceError("%s: amount must not be zero" % (expense.description,))"""),
    ],
}
