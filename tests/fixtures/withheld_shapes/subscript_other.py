import pandas as pd


def run(rows) -> None:
    frame = pd.DataFrame(data=rows[1:], columns=rows[0])
    values = frame[frame.columns[0]].values.tolist()
