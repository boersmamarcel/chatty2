FIXES = {
    1: [
        ("transit/loader.py",
         """        raw_id = fields[0]
        if not raw_id:
            errors.append("line %d: empty stop id" % lineno)
            continue
        if raw_id in seen:
            errors.append("line %d: duplicate stop %r" % (lineno, _norm_id(raw_id)))
            continue
        seen[raw_id] = lineno
""",
         """        raw_id = fields[0]
        if not raw_id:
            errors.append("line %d: empty stop id" % lineno)
            continue
        if _norm_id(raw_id) in seen:
            errors.append("line %d: duplicate stop %r" % (lineno, _norm_id(raw_id)))
            continue
        seen[_norm_id(raw_id)] = lineno
"""),
        ("transit/loader.py",
         """            a, b, line = fields[0], fields[1], fields[2]
""",
         """            a, b, line = _norm_id(fields[0]), _norm_id(fields[1]), fields[2]
"""),
        ("transit/loader.py",
         """            stop_id = fields[0]
            minutes = None
""",
         """            stop_id = _norm_id(fields[0])
            minutes = None
"""),
    ],
    2: [
        ("transit/dijkstra.py",
         """    _check_stops(network, origin)
    if origin == destination:
        return Route([origin], [], 0)

    counter = itertools.count()
    start = (origin, None)
    best = {start: 0}
    parent = {}
    settled = set()
    heap = [(0, next(counter), origin, None)]
    while heap:
        cost, _, stop, line = heapq.heappop(heap)
        state = (stop, line)
        if state in settled:
            continue
        settled.add(state)
        for link in network.outgoing(stop):
            extra = transfer_penalty if link.line != line else 0
            new_cost = cost + link.minutes + extra
            nxt = (link.b, link.line)
            if new_cost < best.get(nxt, INFINITY):
                best[nxt] = new_cost
                parent[nxt] = (state, link)
                heapq.heappush(heap, (new_cost, next(counter), link.b, link.line))

    arrivals = sorted((best[state], state[1]) for state in best if state[0] == destination)
    if not arrivals:
        raise NoRouteError(origin, destination)
    cost, line = arrivals[0]
    return _rebuild(parent, (destination, line), cost)
""",
         """    _check_stops(network, origin, destination)
    if origin == destination:
        return Route([origin], [], 0)

    # Labels are compared as (cost, changes, stops, lines): the canonical
    # route ordering, so the first label popped at the destination wins.
    counter = itertools.count()
    settled = set()
    heap = [(0, 0, (origin,), (), next(counter), ())]
    while heap:
        cost, changes, stops, lines, _, links = heapq.heappop(heap)
        stop = stops[-1]
        line = lines[-1] if lines else None
        if stop == destination:
            return Route(list(stops), list(links), cost)
        state = (stop, line)
        if state in settled:
            continue
        settled.add(state)
        for link in network.outgoing(stop):
            changed = line is not None and link.line != line
            heapq.heappush(heap, (
                cost + link.minutes + (transfer_penalty if changed else 0),
                changes + (1 if changed else 0),
                stops + (link.b,),
                lines + (link.line,),
                next(counter),
                links + (link,),
            ))
    raise NoRouteError(origin, destination)
"""),
    ],
    3: [
        ("transit/timeutil.py",
         """MAX_HOURS = 24
""",
         """MAX_HOURS = 48
"""),
        ("transit/timeutil.py",
         """    hours = hours % 24
""",
         ""),
        ("transit/timetable.py",
         """                ok = conn.dep > reached + min_transfer
""",
         """                ok = conn.dep >= reached + min_transfer
"""),
    ],
    4: [
        ("transit/fares.py",
         """        return (price * factor).quantize(CENT)
""",
         """        return (price * factor).quantize(CENT, rounding=ROUND_HALF_UP)
"""),
        ("transit/fares.py",
         """    if not zones:
        return 0
    count = 1
    for previous, current in zip(zones, zones[1:]):
        if current != previous:
            count += 1
    return count
""",
         """    return len(set(zones))
"""),
        ("transit/fares.py",
         """    zones = zones_of_route(network, route)
    adult = table.price_for_zones(count_zones(zones))
""",
         """    zones = zones_of_route(network, route)
    for stop_id, zone in zip(route.stops, zones):
        if zone is None:
            raise FareError("stop %r has no zone" % (stop_id,))
    adult = table.price_for_zones(count_zones(zones))
"""),
    ],
    5: [
        ("transit/itinerary.py",
         """    groups = OrderedDict()
    for link in links:
        groups.setdefault(link.line, []).append(link)
    return list(groups.values())
""",
         """    groups = []
    for link in links:
        if groups and groups[-1][-1].line == link.line:
            groups[-1].append(link)
        else:
            groups.append([link])
    return groups
"""),
        ("transit/itinerary.py",
         """from collections import OrderedDict


""",
         """
"""),
        ("transit/formatting.py",
         """    Durations are always given in minutes, e.g. ``"45 min"``.
    \"\"\"
    if minutes < 0:
        raise ValueError("negative duration %r" % (minutes,))
    return "%d min" % minutes
""",
         """    Below one hour: ``"45 min"``; from 60 minutes on: ``"1 h 05 min"``.
    \"\"\"
    if minutes < 0:
        raise ValueError("negative duration %r" % (minutes,))
    if minutes < 60:
        return "%d min" % minutes
    hours, rest = divmod(minutes, 60)
    return "%d h %02d min" % (hours, rest)
"""),
    ],
    6: [
        ("transit/alternatives.py",
         """    for stop_id in (origin, destination):
        if not network.has_stop(stop_id):
            raise UnknownStopError(stop_id)
""",
         """    if k < 1:
        raise ValueError("k must be at least 1")
    for stop_id in (origin, destination):
        if not network.has_stop(stop_id):
            raise UnknownStopError(stop_id)
"""),
        ("transit/alternatives.py",
         """and not cost < best_cost + max_extra:
""",
         """and cost > best_cost + max_extra:
"""),
        ("transit/alternatives.py",
         """            if len(stops) > 1 and link.b == stops[-2]:
                continue  # never turn straight back
""",
         """            if link.b in stops:
                continue  # loopless: never visit a stop twice
"""),
    ],
    7: [
        ("transit/validate.py",
         """    adjacency = _adjacency(network)
    seen = set([start])
    queue = deque([start])
    while queue:
        here = queue.popleft()
        for nxt in sorted(adjacency[here]):
""",
         """    seen = set([start])
    queue = deque([start])
    while queue:
        here = queue.popleft()
        for nxt in network.neighbours(here):
"""),
        ("transit/validate.py",
         """    seen = reachable_from(network, root)
    return sorted(stop_id for stop_id in network.stop_ids() if stop_id not in seen)
""",
         """    seen = reachable_from(network, root)
    isolated = set(isolated_stops(network))
    return sorted(stop_id for stop_id in network.stop_ids()
                  if stop_id not in seen and stop_id not in isolated)
"""),
        ("transit/validate.py",
         """    problems = []
    for stop_id in isolated_stops(network):
        problems.append("isolated stop %r" % (stop_id,))
    root = root_stop(network)
    for stop_id in unreachable_stops(network):
        problems.append("stop %r cannot be reached from %r" % (stop_id, root))
    return problems
""",
         """    problems = []
    for stop_id in isolated_stops(network):
        problems.append((stop_id, "isolated stop %r" % (stop_id,)))
    root = root_stop(network)
    for stop_id in unreachable_stops(network):
        problems.append((stop_id, "stop %r cannot be reached from %r" % (stop_id, root)))
    problems.sort()
    return [message for _stop_id, message in problems]
"""),
        ("transit/cli.py",
         """    if not problems:
        print("OK: %d stops, %d links" % (len(network), network.link_count), file=out)
    return 0
""",
         """    if problems:
        return 1
    print("OK: %d stops, %d links" % (len(network), network.link_count), file=out)
    return 0
"""),
    ],
}
