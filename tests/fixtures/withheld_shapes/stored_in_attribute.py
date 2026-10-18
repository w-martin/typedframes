import pandas as pd


def load() -> pd.DataFrame:
    return pd.read_csv("a.csv")


class Holder:
    def run(self) -> None:
        frame = load()
        self.cached = frame
