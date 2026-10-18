import pandas as pd


class Reader:
    def run(self, source, helper) -> None:
        raw = pd.read_csv(helper(f"{source}s"))
        self.consume(raw["price"].values, source + "x" + self.suffix)
