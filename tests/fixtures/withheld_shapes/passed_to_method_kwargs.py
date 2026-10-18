import pandas as pd


def build(frame, other, level="a", scale=1):
    return frame["price"] * scale


def run() -> None:
    frame = pd.read_csv("prices.csv")
    other = pd.read_csv("other.csv")
    built = build(frame, other, level="c", scale=2)
    combined = pd.concat([built, built])
