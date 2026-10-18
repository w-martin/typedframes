import pandas as pd


def load() -> pd.DataFrame:
    return pd.read_csv("a.csv")


def run(flag, fallback) -> None:
    frame = load()

    def pick():
        return frame if flag == 1 else fallback

    pick()
