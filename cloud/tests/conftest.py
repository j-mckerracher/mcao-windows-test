import pytest

from flows import World, make_world


@pytest.fixture
def world() -> World:
    return make_world()
