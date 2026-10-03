"""Walk a template AST and produce the output text.

The :class:`Renderer` evaluates expression nodes against a
:class:`~stencil.runtime.Context` and appends output strings to a list.
Statements that introduce variables (``for``, ``with``, includes) push a new
scope and pop it afterwards, so their variables do not leak; ``{% set %}``
assigns in the current innermost scope.
"""

import operator

from . import nodes
from .errors import TemplateNotFound, TemplateRuntimeError
from .escaping import Markup, escape
from .runtime import LoopContext, Undefined
from .utils import to_text

BINARY_OPS = {
    "+": operator.add,
    "-": operator.sub,
    "*": operator.mul,
    "/": operator.truediv,
    "//": operator.floordiv,
    "%": operator.mod,
}

COMPARE_OPS = {
    "==": operator.eq,
    "!=": operator.ne,
    "<": operator.lt,
    "<=": operator.le,
    ">": operator.gt,
    ">=": operator.ge,
    "in": lambda a, b: a in b,
    "notin": lambda a, b: a not in b,
}


class Renderer(object):
    """Render templates of one environment.

    :param environment: the owning :class:`~stencil.environment.Environment`.
    :param autoescape: escape printed values (bool).
    :param include_depth: nesting level of includes (guards include loops).
    """

    def __init__(self, environment, autoescape=False, include_depth=0):
        self.environment = environment
        self.autoescape = autoescape
        self.include_depth = include_depth

    # --------------------------------------------------------------- output

    def render(self, template_node, context):
        out = []
        self.render_nodes(template_node.body, context, out)
        return "".join(out)

    def render_nodes(self, body, context, out):
        for node in body:
            method = getattr(self, "visit_" + type(node).__name__, None)
            if method is None:
                raise TemplateRuntimeError("cannot render %s" % type(node).__name__)
            method(node, context, out)

    def to_output(self, value):
        """Convert a value to the string that is written to the output."""
        if isinstance(value, Undefined):
            return str(value)
        if value is None:
            return ""
        if self.autoescape:
            return str(escape(value))
        return to_text(value)

    # ----------------------------------------------------------- statements

    def visit_Text(self, node, context, out):
        out.append(node.data)

    def visit_Output(self, node, context, out):
        out.append(self.to_output(self.evaluate(node.expr, context)))

    def visit_If(self, node, context, out):
        for test, body in node.branches:
            if self.evaluate(test, context):
                self.render_nodes(body, context, out)
                return
        self.render_nodes(node.else_, context, out)

    def visit_Set(self, node, context, out):
        context.set(node.name, self.evaluate(node.expr, context))

    def visit_With(self, node, context, out):
        values = [(name, self.evaluate(expr, context)) for name, expr in node.assignments]
        context.push(dict(values))
        try:
            self.render_nodes(node.body, context, out)
        finally:
            context.pop()

    def assign_targets(self, targets, item):
        """The variables bound by one iteration of ``{% for targets in ... %}``."""
        if len(targets) == 1:
            return {targets[0]: item}
        try:
            values = list(item)
        except TypeError:
            raise TemplateRuntimeError("cannot unpack %r into %s" % (item, ", ".join(targets)))
        if len(values) != len(targets):
            raise TemplateRuntimeError("cannot unpack %d values into %d names" % (len(values), len(targets)))
        return dict(zip(targets, values))

    def visit_For(self, node, context, out):
        iterable = self.evaluate(node.iter, context)
        if isinstance(iterable, dict):
            iterable = list(iterable)
        outer = context.resolve("loop")
        depth0 = outer.depth0 + 1 if isinstance(outer, LoopContext) else 0
        loop = LoopContext(iterable, depth0, context.undefined)
        iterated = False
        for item in loop:
            iterated = True
            scope = self.assign_targets(node.targets, item)
            scope["loop"] = loop
            context.push(scope)
            try:
                self.render_nodes(node.body, context, out)
            finally:
                context.pop()
        if not iterated:
            self.render_nodes(node.else_, context, out)

    def visit_Include(self, node, context, out):
        if self.include_depth >= self.environment.max_include_depth:
            raise TemplateRuntimeError("include depth exceeded (recursive include?)")
        names = self.evaluate(node.template, context)
        if isinstance(names, str):
            names = [names]
        template = None
        error = None
        for name in names:
            try:
                template = self.environment.get_template(name)
                break
            except TemplateNotFound as exc:
                error = exc
        if template is None:
            if node.ignore_missing:
                return
            raise error or TemplateNotFound("", "include: no template names given")
        if node.with_context:
            target = context
            target.push()
        else:
            target = context.derived()
            target.push()
        renderer = Renderer(self.environment, template.autoescape, self.include_depth + 1)
        try:
            renderer.render_nodes(template.ast.body, target, out)
        finally:
            target.pop()

    def visit_FilterBlock(self, node, context, out):
        buffer = []
        self.render_nodes(node.body, context, buffer)
        value = "".join(buffer)
        if self.autoescape:
            value = Markup(value)
        for name, args, kwargs in node.filters:
            value = self.call_filter(name, value, args, kwargs, context)
        out.append(self.to_output(value))

    # ---------------------------------------------------------- expressions

    def evaluate(self, node, context):
        method = getattr(self, "eval_" + type(node).__name__, None)
        if method is None:
            raise TemplateRuntimeError("cannot evaluate %s" % type(node).__name__)
        return method(node, context)

    def eval_Const(self, node, context):
        return node.value

    def eval_Name(self, node, context):
        return context.resolve(node.name)

    def eval_List(self, node, context):
        return [self.evaluate(item, context) for item in node.items]

    def eval_Dict(self, node, context):
        return dict((self.evaluate(k, context), self.evaluate(v, context)) for k, v in node.items)

    def eval_Getattr(self, node, context):
        obj = self.evaluate(node.node, context)
        return self.getattr(obj, node.attr, context)

    def eval_Getitem(self, node, context):
        obj = self.evaluate(node.node, context)
        key = self.evaluate(node.arg, context)
        return self.getitem(obj, key, context)

    def getattr(self, obj, attr, context):
        """``obj.attr``: attribute first, then item; undefined when neither."""
        if attr.startswith("_"):
            return context.undefined(attr)
        if isinstance(obj, Undefined):
            return getattr(obj, attr)
        try:
            return getattr(obj, attr)
        except AttributeError:
            pass
        try:
            return obj[attr]
        except (KeyError, IndexError, TypeError):
            return context.undefined(attr)

    def getitem(self, obj, key, context):
        """``obj[key]``: item first, then attribute for string keys."""
        if isinstance(obj, Undefined):
            return obj[key]
        try:
            return obj[key]
        except (KeyError, IndexError, TypeError):
            pass
        if isinstance(key, str) and not key.startswith("_"):
            try:
                return getattr(obj, key)
            except AttributeError:
                pass
        return context.undefined(str(key))

    def eval_Call(self, node, context):
        func = self.evaluate(node.node, context)
        args = [self.evaluate(arg, context) for arg in node.args]
        kwargs = dict((key, self.evaluate(value, context)) for key, value in node.kwargs)
        if not callable(func):
            raise TemplateRuntimeError("%r is not callable" % (func,))
        return func(*args, **kwargs)

    def call_filter(self, name, value, args, kwargs, context):
        func = self.environment.filters.get(name)
        if func is None:
            raise TemplateRuntimeError("no filter named '%s'" % name)
        args = [self.evaluate(arg, context) for arg in args]
        kwargs = dict((key, self.evaluate(expr, context)) for key, expr in kwargs)
        return func(value, *args, **kwargs)

    def eval_Filter(self, node, context):
        value = self.evaluate(node.node, context)
        return self.call_filter(node.name, value, node.args, node.kwargs, context)

    def eval_Test(self, node, context):
        func = self.environment.tests.get(node.name)
        if func is None:
            raise TemplateRuntimeError("no test named '%s'" % node.name)
        value = self.evaluate(node.node, context)
        args = [self.evaluate(arg, context) for arg in node.args]
        result = bool(func(value, *args))
        return not result if node.negated else result

    def eval_BinOp(self, node, context):
        left = self.evaluate(node.left, context)
        right = self.evaluate(node.right, context)
        try:
            return BINARY_OPS[node.op](left, right)
        except ZeroDivisionError:
            raise TemplateRuntimeError("division by zero")
        except TypeError as exc:
            raise TemplateRuntimeError("cannot apply '%s': %s" % (node.op, exc))

    def eval_Concat(self, node, context):
        return "".join(to_text(self.evaluate(item, context)) for item in node.nodes)

    def eval_Neg(self, node, context):
        return -self.evaluate(node.node, context)

    def eval_Not(self, node, context):
        return not self.evaluate(node.node, context)

    def eval_And(self, node, context):
        left = self.evaluate(node.left, context)
        if not left:
            return left
        return self.evaluate(node.right, context)

    def eval_Or(self, node, context):
        left = self.evaluate(node.left, context)
        if left:
            return left
        return self.evaluate(node.right, context)

    def eval_Compare(self, node, context):
        left = self.evaluate(node.expr, context)
        for op, expr in node.ops:
            right = self.evaluate(expr, context)
            if not COMPARE_OPS[op](left, right):
                return False
            left = right
        return True
