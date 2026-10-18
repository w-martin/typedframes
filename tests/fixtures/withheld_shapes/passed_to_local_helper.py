import pandas as pd


def summarise(frame):
    return frame["price"].sum()


def run() -> None:
    frame = pd.read_csv("prices.csv")
    summarise(frame)
