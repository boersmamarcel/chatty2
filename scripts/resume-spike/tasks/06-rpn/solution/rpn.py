import operator

OPS = {"+": operator.add, "-": operator.sub, "*": operator.mul, "/": operator.truediv}


def evaluate(expression):
    """Evaluate a reverse-Polish expression."""
    stack = []
    for token in expression.split():
        if token in OPS:
            if len(stack) < 2:
                raise ValueError("operator %s needs two operands" % token)
            b, a = stack.pop(), stack.pop()
            stack.append(OPS[token](a, b))
        else:
            try:
                stack.append(int(token))
            except ValueError:
                raise ValueError("unknown token %r" % token)
    if len(stack) != 1:
        raise ValueError("expression leaves %d values" % len(stack))
    return stack[0]
