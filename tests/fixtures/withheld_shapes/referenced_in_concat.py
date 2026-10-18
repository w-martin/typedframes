import pandas as pd


def part(a, b, level="x") -> pd.DataFrame:
    return pd.DataFrame(a)


def run(a, b) -> None:
    first = part(a, b, level="c1")
    second = part(b, a, level="c2")
    combined = pd.concat([first, second])
