import pandas as pd


def load() -> pd.DataFrame:
    return pd.read_csv("a.csv")


class Holder:
    frame = load()

    def run(self, holder) -> None:
        holder.frame = load()
        holder.table["x"] = load()
