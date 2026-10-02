"""Page through a list, pages numbered from 1."""


def page_count(n_items, per_page):
    """How many pages n_items fill; an empty list still has one (empty) page."""
    if n_items == 0:
        return 1
    return -(-n_items // per_page)


def paginate(items, page, per_page):
    """The items on page `page` (1-based); a page past the end is empty."""
    if page < 1:
        raise ValueError("pages start at 1")
    start = (page - 1) * per_page
    return items[start:start + per_page]
