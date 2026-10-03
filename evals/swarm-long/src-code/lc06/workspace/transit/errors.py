"""Exception hierarchy of the transit package.

Every error raised on purpose by the package derives from :class:`TransitError`
so that callers (the command line tool in particular) can catch them in one
place and report them without a traceback.
"""


class TransitError(Exception):
    """Base class of every error raised by the transit package."""


class NetworkFormatError(TransitError):
    """The network description could not be parsed.

    The loader does not stop at the first problem: it collects every problem
    it finds and raises one ``NetworkFormatError`` at the end.  The individual
    messages are kept in :attr:`errors` (each one prefixed with ``line N:``);
    ``str(error)`` joins them with newlines.
    """

    def __init__(self, errors):
        self.errors = list(errors)
        super(NetworkFormatError, self).__init__("\n".join(self.errors))


class TimetableError(TransitError):
    """A timetable file is malformed (bad time, missing column, ...)."""


class UnknownStopError(TransitError, LookupError):
    """A stop id was used that the network does not know."""

    def __init__(self, stop_id):
        self.stop_id = stop_id
        super(UnknownStopError, self).__init__("unknown stop %r" % (stop_id,))

    def __str__(self):
        return "unknown stop %r" % (self.stop_id,)


class NoRouteError(TransitError):
    """There is no route between two (known) stops."""

    def __init__(self, origin, destination):
        self.origin = origin
        self.destination = destination
        super(NoRouteError, self).__init__(
            "no route from %r to %r" % (origin, destination))


class FareError(TransitError):
    """A fare could not be computed (missing zone, unknown concession, ...)."""
