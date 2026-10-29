from calc import clamp


def test_clamp():
    assert clamp(-3) == 0
    assert clamp(5) == 5
