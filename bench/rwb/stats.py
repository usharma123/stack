"""Descriptive statistics over successful samples only. Nearest-rank percentiles."""
import math


def nearest_rank(values, percentile):
    if not values:
        return None
    if not 0 < percentile <= 100:
        raise ValueError("percentile must be in (0, 100]")
    ordered = sorted(values)
    return ordered[max(1, math.ceil(percentile / 100 * len(ordered))) - 1]


def summarize_ns(samples):
    """samples: list of (ok, ns). Failures are counted, never folded into the distribution."""
    good = [ns for ok, ns in samples if ok and ns is not None]
    ms = lambda v: None if v is None else round(v / 1e6, 3)
    return dict(n=len(samples), ok=len(good), failed=len(samples) - len(good),
                p50_ms=ms(nearest_rank(good, 50)), p95_ms=ms(nearest_rank(good, 95)),
                min_ms=ms(min(good) if good else None), max_ms=ms(max(good) if good else None))
