class Cookie:
    name = "sid"
    value = "cookie-secret"
    domain = "uai.example"
    path = "/"
    secure = True
    expires = None


class Cookies:
    def __iter__(self):
        return iter([Cookie()])

    def set(self, *args, **kwargs):
        return None


class Response:
    def __init__(self, value):
        self._value = value

    def json(self):
        return self._value


class Session:
    def __init__(self):
        self.headers = {}
        self.cookies = Cookies()

    def get(self, url, **kwargs):
        if "getCourseListByStudent" in url:
            return Response({"value": {"courseList": [{"id": 7, "name": "Course", "classId": "class-1", "courseResourceList": [{"id": 9, "name": "Book"}]}]}})
        if "getCourseResourceInfoById" in url:
            return Response({"value": {"courseResource": {"courseInstanceId": "instance-1"}}})
        if "unitTaskSituation" in url:
            return Response({"success": True, "value": {"list": [{
                "nodeId": "unit-1", "role": "unit", "children": [{
                    "nodeId": "task-1", "role": "link", "finishProgress": 50,
                    "duration": 321, "required": True,
                }],
            }]}})
        if "totalAndUnitSituation" in url:
            return Response({"success": True, "value": {"totalDetail": {"duration": 654}}})
        raise AssertionError(url)


UAI_HOST = "uai.example"
ACCOUNT_USERNAME = ""
ACCOUNT_PASSWORD = ""
AI_ENABLED = True


class UnipusBot:
    def __init__(self):
        self.session = Session()
        self.open_id = None
        self.user_id = None
        self.sso_id = None
        self.task_completion = {}
        self.processed = False

    def login(self):
        print(f"login {ACCOUNT_USERNAME} {ACCOUNT_PASSWORD}")
        if ACCOUNT_USERNAME == "raise-secret":
            raise RuntimeError(f"unexpected {ACCOUNT_USERNAME} {ACCOUNT_PASSWORD}")
        self.session.headers["Authorization"] = "jwt-secret"
        self.open_id = "open-1"
        self.user_id = "user-1"
        self.sso_id = "sso-1"
        return True

    def fetch_structure(self):
        return True

    def fetch_task_completion(self):
        self.task_completion["task-1"] = self.processed

    def collect_groups(self):
        group = {"id": "task-1", "name": "Question", "role": "group", "base": "single-choice", "question_num": 1}
        unit = {"id": "unit-1", "name": "Unit", "caption": "1", "role": "unit"}
        return [(group, [unit, group])]

    def classify_task(self, base):
        return "objective"

    def get_content(self, task_id):
        return {"id": task_id, "children": [{"id": "question-1", "type": "single-choice", "question": "Fixture question", "options": ["A", "B"]}]}

    def get_answer(self, task_id):
        return [{"id": "question-1", "answer": "answer-native"}]

    def build_simple_submit_body(self, *args):
        return {}

    def submit(self, body):
        return True

    def process_task(self, group, ancestors):
        self.processed = True
        return True
